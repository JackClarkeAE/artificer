use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::topology::{
    CoedgeKey, Curve2, Curve3, EdgeKey, EntityId, Face, FaceKey, LoopKey, Orientation, Point2,
    Point3, SolidKey, Surface, Topology, TopologyCounts, Vector2, Vector3,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticCode {
    DanglingEntityReference,
    EdgeEndpointMismatch,
    CurveFrameInvalid,
    ParameterRangeInvalid,
    EntityNotFinite,
    FaceOrientationInvalid,
    FaceLoopIntersection,
    FaceHoleOutside,
    LoopNotClosed,
    LoopTooShort,
    PcurveEndpointMismatch,
    PcurveLocusMismatch,
    SolidVolumeNonPositive,
    SurfaceFrameInvalid,
    EdgeUseCount,
    EdgeUseOrientation,
    CoedgeUseCount,
    LoopUseCount,
    FaceUseCount,
    ShellUseCount,
    ShellDisconnected,
    EulerCharacteristicInvalid,
}

impl DiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DanglingEntityReference => "DANGLING_ENTITY_REFERENCE",
            Self::EdgeEndpointMismatch => "EDGE_ENDPOINT_MISMATCH",
            Self::CurveFrameInvalid => "CURVE_FRAME_INVALID",
            Self::ParameterRangeInvalid => "PARAMETER_RANGE_INVALID",
            Self::EntityNotFinite => "ENTITY_NOT_FINITE",
            Self::FaceOrientationInvalid => "FACE_ORIENTATION_INVALID",
            Self::FaceLoopIntersection => "FACE_LOOP_INTERSECTION",
            Self::FaceHoleOutside => "FACE_HOLE_OUTSIDE",
            Self::LoopNotClosed => "LOOP_NOT_CLOSED",
            Self::LoopTooShort => "LOOP_TOO_SHORT",
            Self::PcurveEndpointMismatch => "PCURVE_ENDPOINT_MISMATCH",
            Self::PcurveLocusMismatch => "PCURVE_LOCUS_MISMATCH",
            Self::SolidVolumeNonPositive => "SOLID_VOLUME_NON_POSITIVE",
            Self::SurfaceFrameInvalid => "SURFACE_FRAME_INVALID",
            Self::EdgeUseCount => "EDGE_USE_COUNT",
            Self::EdgeUseOrientation => "EDGE_USE_ORIENTATION",
            Self::CoedgeUseCount => "COEDGE_USE_COUNT",
            Self::LoopUseCount => "LOOP_USE_COUNT",
            Self::FaceUseCount => "FACE_USE_COUNT",
            Self::ShellUseCount => "SHELL_USE_COUNT",
            Self::ShellDisconnected => "SHELL_DISCONNECTED",
            Self::EulerCharacteristicInvalid => "EULER_CHARACTERISTIC_INVALID",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub path: String,
    pub measured: Option<f64>,
    pub allowed: Option<f64>,
}

impl Diagnostic {
    fn new(code: DiagnosticCode, path: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            measured: None,
            allowed: None,
        }
    }

    fn with_measure(mut self, measured: f64, allowed: f64) -> Self {
        self.measured = Some(measured);
        self.allowed = Some(allowed);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds3 {
    pub min: Point3,
    pub max: Point3,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShapeMeasures {
    pub bounds: Option<Bounds3>,
    pub surface_area: f64,
    pub signed_volume: f64,
    pub centroid: Option<Point3>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidationReport {
    pub diagnostics: Vec<Diagnostic>,
    pub counts: TopologyCounts,
    pub measures: ShapeMeasures,
}

impl ValidationReport {
    #[cfg(test)]
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

pub(crate) fn validate(topology: &Topology, linear_tolerance: f64) -> ValidationReport {
    artificer_compute::perf_span!("kernel.validate", topology.faces.len(), {
        validate_with_pool(
            artificer_compute::ComputePool::global(),
            topology,
            linear_tolerance,
        )
    })
}

pub(crate) fn validate_with_pool(
    compute: &artificer_compute::ComputePool,
    topology: &Topology,
    linear_tolerance: f64,
) -> ValidationReport {
    // These validation families only read immutable topology. Keep each
    // family's local diagnostic order, then perform the canonical global sort
    // below so thread scheduling can never affect a published report.
    let families = [0_u8, 1, 2, 3];
    let workload_items = topology.vertices.len()
        + topology.edges.len()
        + topology.coedges.len()
        + topology.loops.len()
        + topology.faces.len()
        + topology.shells.len()
        + topology.solids.len();
    let mut diagnostics = compute.flat_map_with_workload(
        "kernel.validation.families",
        &families,
        workload_items,
        |_, family| {
            let mut diagnostics = Vec::new();
            match family {
                0 => validate_geometry(topology, linear_tolerance, &mut diagnostics),
                1 => validate_references_and_loops(topology, linear_tolerance, &mut diagnostics),
                2 => validate_edge_uses(topology, &mut diagnostics),
                3 => validate_ownership_and_shells(topology, &mut diagnostics),
                _ => unreachable!("validation family is fixed"),
            }
            diagnostics
        },
    );

    let measures = calculate_measures(topology);
    if !topology.solids.is_empty()
        && (!measures.signed_volume.is_finite()
            || measures.signed_volume <= linear_tolerance.powi(3))
    {
        diagnostics.push(
            Diagnostic::new(DiagnosticCode::SolidVolumeNonPositive, "shape/solid-volume")
                .with_measure(measures.signed_volume, linear_tolerance.powi(3)),
        );
    }

    diagnostics.sort_by(|left, right| {
        left.code
            .as_str()
            .cmp(right.code.as_str())
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| {
                left.measured
                    .map(f64::to_bits)
                    .cmp(&right.measured.map(f64::to_bits))
            })
    });

    ValidationReport {
        diagnostics,
        counts: TopologyCounts::from(topology),
        measures,
    }
}

fn validate_geometry(
    topology: &Topology,
    linear_tolerance: f64,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for vertex in &topology.vertices {
        if !vertex.value.point.is_finite() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::EntityNotFinite,
                format!("vertex/{}", vertex.id.get()),
            ));
        }
    }

    for edge in &topology.edges {
        if !edge.value.curve.is_finite() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::EntityNotFinite,
                format!("edge/{}/curve", edge.id.get()),
            ));
        }
        let range = edge.value.parameter_range;
        if !range.is_finite() || range.start == range.end {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::ParameterRangeInvalid,
                format!("edge/{}/parameter-range", edge.id.get()),
            ));
        }
        let curve_frame_error = match edge.value.curve {
            Curve3::Line { endpoints } => {
                let length = endpoints[0].distance(endpoints[1]);
                if length > linear_tolerance {
                    0.0
                } else {
                    linear_tolerance - length
                }
            }
            Curve3::Circle { u, v, radius, .. } => (u.length() - 1.0)
                .abs()
                .max((v.length() - 1.0).abs())
                .max(u.dot(v).abs())
                .max((u.cross(v).length() - 1.0).abs())
                .max(if radius > linear_tolerance {
                    0.0
                } else {
                    linear_tolerance - radius
                }),
            Curve3::Ellipse {
                u,
                v,
                major_radius,
                minor_radius,
                ..
            } => (u.length() - 1.0)
                .abs()
                .max((v.length() - 1.0).abs())
                .max(u.dot(v).abs())
                .max((u.cross(v).length() - 1.0).abs())
                .max(if minor_radius > linear_tolerance {
                    0.0
                } else {
                    linear_tolerance - minor_radius
                })
                // The major axis is the first one by construction.
                .max((minor_radius - major_radius).max(0.0)),
        };
        if !curve_frame_error.is_finite() || curve_frame_error > linear_tolerance {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::CurveFrameInvalid,
                    format!("edge/{}/curve", edge.id.get()),
                )
                .with_measure(curve_frame_error, linear_tolerance),
            );
        }
        if matches!(edge.value.curve, Curve3::Line { .. })
            && edge.value.parameter_range != crate::topology::ParameterRange::new(0.0, 1.0)
        {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::ParameterRangeInvalid,
                format!("edge/{}/parameter-range", edge.id.get()),
            ));
        }
        if matches!(
            edge.value.curve,
            Curve3::Circle { .. } | Curve3::Ellipse { .. }
        ) {
            let sweep = (range.end - range.start).abs();
            if !sweep.is_finite()
                || sweep <= f64::EPSILON
                || sweep > std::f64::consts::TAU + linear_tolerance
            {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::ParameterRangeInvalid,
                        format!("edge/{}/parameter-range", edge.id.get()),
                    )
                    .with_measure(sweep, std::f64::consts::TAU),
                );
            }
        }
        let curve_endpoints = edge.value.endpoints();
        for (endpoint_index, endpoint) in curve_endpoints.iter().enumerate() {
            if !endpoint.is_finite() {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::EntityNotFinite,
                    format!("edge/{}/curve/{endpoint_index}", edge.id.get()),
                ));
            }
        }

        for (endpoint_index, (&vertex_key, curve_endpoint)) in edge
            .value
            .vertices
            .iter()
            .zip(curve_endpoints.iter())
            .enumerate()
        {
            let Some(vertex) = topology.vertex(vertex_key) else {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::DanglingEntityReference,
                    format!("edge/{}/vertex/{endpoint_index}", edge.id.get()),
                ));
                continue;
            };
            let distance = vertex.value.point.distance(*curve_endpoint);
            if !distance.is_finite() || distance > linear_tolerance {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::EdgeEndpointMismatch,
                        format!("edge/{}/vertex/{endpoint_index}", edge.id.get()),
                    )
                    .with_measure(distance, linear_tolerance),
                );
            }
        }
    }

    for coedge in &topology.coedges {
        if topology.edge(coedge.value.edge).is_none() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::DanglingEntityReference,
                format!("coedge/{}/edge", coedge.id.get()),
            ));
        }
        if !coedge.value.pcurve.is_finite() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::EntityNotFinite,
                format!("coedge/{}/pcurve", coedge.id.get()),
            ));
        }
        if !coedge.value.parameter_range.is_finite()
            || coedge.value.parameter_range.start == coedge.value.parameter_range.end
        {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::ParameterRangeInvalid,
                format!("coedge/{}/parameter-range", coedge.id.get()),
            ));
        }
        if let Curve2::Circle { u, v, radius, .. } = coedge.value.pcurve {
            let frame_error = (u.x.hypot(u.y) - 1.0)
                .abs()
                .max((v.x.hypot(v.y) - 1.0).abs())
                .max((u.x * v.x + u.y * v.y).abs())
                .max(((u.x * v.y - u.y * v.x).abs() - 1.0).abs())
                .max(if radius > linear_tolerance {
                    0.0
                } else {
                    linear_tolerance - radius
                });
            if !frame_error.is_finite() || frame_error > linear_tolerance {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::CurveFrameInvalid,
                        format!("coedge/{}/pcurve", coedge.id.get()),
                    )
                    .with_measure(frame_error, linear_tolerance),
                );
            }
            let sweep =
                (coedge.value.parameter_range.end - coedge.value.parameter_range.start).abs();
            if !sweep.is_finite()
                || sweep <= f64::EPSILON
                || sweep > std::f64::consts::TAU + linear_tolerance
            {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::ParameterRangeInvalid,
                        format!("coedge/{}/parameter-range", coedge.id.get()),
                    )
                    .with_measure(sweep, std::f64::consts::TAU),
                );
            }
        } else if let Curve2::Ellipse {
            u,
            v,
            major_radius,
            minor_radius,
            ..
        } = coedge.value.pcurve
        {
            let frame_error = (u.x.hypot(u.y) - 1.0)
                .abs()
                .max((v.x.hypot(v.y) - 1.0).abs())
                .max((u.x * v.x + u.y * v.y).abs())
                .max(((u.x * v.y - u.y * v.x).abs() - 1.0).abs())
                .max(
                    if minor_radius > linear_tolerance && major_radius >= minor_radius {
                        0.0
                    } else {
                        linear_tolerance
                    },
                );
            if !frame_error.is_finite() || frame_error > linear_tolerance {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::CurveFrameInvalid,
                        format!("coedge/{}/pcurve", coedge.id.get()),
                    )
                    .with_measure(frame_error, linear_tolerance),
                );
            }
            let sweep =
                (coedge.value.parameter_range.end - coedge.value.parameter_range.start).abs();
            if !sweep.is_finite()
                || sweep <= f64::EPSILON
                || sweep > std::f64::consts::TAU + linear_tolerance
            {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::ParameterRangeInvalid,
                        format!("coedge/{}/parameter-range", coedge.id.get()),
                    )
                    .with_measure(sweep, std::f64::consts::TAU),
                );
            }
        } else if let Curve2::Harmonic { amplitude, .. } = coedge.value.pcurve {
            // The azimuth is the parameter, so any finite span up to one
            // turn is well-formed; a flat harmonic is a line in disguise.
            let sweep =
                (coedge.value.parameter_range.end - coedge.value.parameter_range.start).abs();
            if !sweep.is_finite()
                || sweep <= f64::EPSILON
                || sweep > std::f64::consts::TAU + linear_tolerance
                || amplitude.abs() <= linear_tolerance
            {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::ParameterRangeInvalid,
                        format!("coedge/{}/parameter-range", coedge.id.get()),
                    )
                    .with_measure(sweep, std::f64::consts::TAU),
                );
            }
        } else if coedge.value.parameter_range != crate::topology::ParameterRange::new(0.0, 1.0) {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::ParameterRangeInvalid,
                format!("coedge/{}/parameter-range", coedge.id.get()),
            ));
        }
        let pcurve_endpoints = coedge.value.pcurve_endpoints();
        for (endpoint_index, endpoint) in pcurve_endpoints.iter().enumerate() {
            if !endpoint.is_finite() {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::EntityNotFinite,
                    format!("coedge/{}/pcurve/{endpoint_index}", coedge.id.get()),
                ));
            }
        }
    }

    for face in &topology.faces {
        if !face.value.surface.is_finite() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::EntityNotFinite,
                format!("face/{}/plane", face.id.get()),
            ));
            continue;
        }
        let frame_error = match face.value.surface {
            Surface::Plane(plane) => (plane.normal.length() - 1.0)
                .abs()
                .max((plane.u.length() - 1.0).abs())
                .max((plane.v.length() - 1.0).abs())
                .max(plane.u.dot(plane.v).abs()),
            Surface::Cylinder(cylinder) => (cylinder.axis.length() - 1.0)
                .abs()
                .max((cylinder.radial_u.length() - 1.0).abs())
                .max((cylinder.radial_v.length() - 1.0).abs())
                .max(cylinder.radial_u.dot(cylinder.radial_v).abs())
                .max(cylinder.radial_u.dot(cylinder.axis).abs())
                .max(cylinder.radial_v.dot(cylinder.axis).abs())
                .max((cylinder.angular_sign.abs() - 1.0).abs())
                .max(
                    (cylinder
                        .radial_u
                        .cross(cylinder.radial_v)
                        .dot(cylinder.axis)
                        - 1.0)
                        .abs(),
                )
                .max(if cylinder.radius > linear_tolerance {
                    0.0
                } else {
                    linear_tolerance - cylinder.radius
                }),
            Surface::Torus(torus) => (torus.axis.length() - 1.0)
                .abs()
                .max((torus.radial_u.length() - 1.0).abs())
                .max((torus.radial_v.length() - 1.0).abs())
                .max(torus.radial_u.dot(torus.radial_v).abs())
                .max(torus.radial_u.dot(torus.axis).abs())
                .max(torus.radial_v.dot(torus.axis).abs())
                .max((torus.angular_sign.abs() - 1.0).abs())
                .max(
                    (torus.radial_u.cross(torus.radial_v) - torus.axis * torus.angular_sign)
                        .length(),
                )
                .max(if torus.minor_radius > 0.0 && torus.major_radius > 0.0 {
                    0.0
                } else {
                    f64::INFINITY
                })
                // A torus self-intersects only where its ring radius reaches
                // the axis. A blend band over a tight arc can have a minor
                // radius above its major one and still be a sound patch, so
                // the condition is checked over the face's own minor-angle
                // extent rather than over the whole surface.
                .max(
                    pcurve_extent(topology, &face.value).map_or(0.0, |(_, _, v_min, v_max)| {
                        let ring = |angle: f64| {
                            torus.minor_radius.mul_add(angle.cos(), torus.major_radius)
                        };
                        let mut smallest = ring(v_min).min(ring(v_max));
                        // The minimum sits at v = pi when that lies inside.
                        let turns = (v_min / std::f64::consts::TAU).floor() as i32 - 1;
                        for step in turns..=turns + 3 {
                            let cusp =
                                std::f64::consts::PI + std::f64::consts::TAU * f64::from(step);
                            if (v_min..=v_max).contains(&cusp) {
                                smallest = smallest.min(ring(cusp));
                            }
                        }
                        if smallest > 0.0 { 0.0 } else { f64::INFINITY }
                    }),
                ),
            Surface::Sphere(sphere) => (sphere.axis.length() - 1.0)
                .abs()
                .max((sphere.radial_u.length() - 1.0).abs())
                .max((sphere.radial_v.length() - 1.0).abs())
                .max(sphere.radial_u.dot(sphere.radial_v).abs())
                .max(sphere.radial_u.dot(sphere.axis).abs())
                .max(sphere.radial_v.dot(sphere.axis).abs())
                .max((sphere.angular_sign.abs() - 1.0).abs())
                .max(
                    (sphere.radial_u.cross(sphere.radial_v) - sphere.axis * sphere.angular_sign)
                        .length(),
                )
                .max(if sphere.radius > 0.0 {
                    0.0
                } else {
                    f64::INFINITY
                }),
            Surface::Cone(cone) => (cone.axis.length() - 1.0)
                .abs()
                .max((cone.radial_u.length() - 1.0).abs())
                .max((cone.radial_v.length() - 1.0).abs())
                .max(cone.radial_u.dot(cone.radial_v).abs())
                .max(cone.radial_u.dot(cone.axis).abs())
                .max(cone.radial_v.dot(cone.axis).abs())
                .max((cone.angular_sign.abs() - 1.0).abs())
                // As with a cylinder, the frame stays right-handed and the
                // angular sign alone decides which way the surface faces, so a
                // band whose material lies inside the cone can be expressed.
                .max((cone.radial_u.cross(cone.radial_v).dot(cone.axis) - 1.0).abs())
                .max(if cone.base_radius > 0.0 && cone.slope.is_finite() {
                    0.0
                } else {
                    f64::INFINITY
                }),
        };
        if frame_error > linear_tolerance {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::SurfaceFrameInvalid,
                    format!("face/{}/plane", face.id.get()),
                )
                .with_measure(frame_error, linear_tolerance),
            );
        }
    }
}

fn validate_references_and_loops(
    topology: &Topology,
    linear_tolerance: f64,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for loop_record in &topology.loops {
        // Two analytic arcs (or one line plus one arc) are a legitimate
        // closed wire. Full circles are split into two semicircles by the
        // constructor, so a one-use loop remains malformed/ambiguous.
        if loop_record.value.coedges.len() < 2 {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::LoopTooShort,
                format!("loop/{}", loop_record.id.get()),
            ));
        }

        let mut endpoints = Vec::with_capacity(loop_record.value.coedges.len());
        for (use_index, coedge_key) in loop_record.value.coedges.iter().enumerate() {
            let Some(coedge) = topology.coedge(*coedge_key) else {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::DanglingEntityReference,
                    format!("loop/{}/coedge/{use_index}", loop_record.id.get()),
                ));
                continue;
            };
            let Some((vertex_keys, _)) = topology.oriented_edge_vertices(&coedge.value) else {
                continue;
            };
            endpoints.push((use_index, vertex_keys));
        }

        if endpoints.len() == loop_record.value.coedges.len() && !endpoints.is_empty() {
            for index in 0..endpoints.len() {
                let next_index = (index + 1) % endpoints.len();
                if endpoints[index].1[1] != endpoints[next_index].1[0] {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::LoopNotClosed,
                        format!(
                            "loop/{}/between/{}/{}",
                            loop_record.id.get(),
                            endpoints[index].0,
                            endpoints[next_index].0
                        ),
                    ));
                }
            }
        }
    }

    for face_record in &topology.faces {
        let face = &face_record.value;
        let mut polygons = Vec::with_capacity(1 + face.inner_loops.len());
        for (loop_index, loop_key) in face.loops().enumerate() {
            let loop_path = if loop_index == 0 {
                "outer-loop".to_owned()
            } else {
                format!("inner-loop/{}", loop_index - 1)
            };
            let Some(loop_record) = topology.loop_record(loop_key) else {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::DanglingEntityReference,
                    format!("face/{}/{}", face_record.id.get(), loop_path),
                ));
                continue;
            };

            validate_face_pcurves(
                topology,
                face_record.id,
                face,
                &loop_record.value.coedges,
                &loop_path,
                linear_tolerance,
                diagnostics,
            );

            let orientation = loop_parameter_area(topology, loop_key).unwrap_or(f64::NAN);
            let oriented_area = if loop_index == 0 {
                orientation
            } else {
                -orientation
            };
            if !oriented_area.is_finite() || oriented_area <= linear_tolerance.powi(2) {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::FaceOrientationInvalid,
                        format!("face/{}/{}", face_record.id.get(), loop_path),
                    )
                    .with_measure(oriented_area, linear_tolerance.powi(2)),
                );
            }
            if face.surface.as_plane().is_some()
                && loop_record.value.coedges.iter().all(|coedge_key| {
                    topology
                        .coedge(*coedge_key)
                        .is_some_and(|coedge| matches!(coedge.value.pcurve, Curve2::Line { .. }))
                })
                && let Some(polygon) = face_polygon(topology, loop_key)
            {
                polygons.push(polygon);
            }
        }

        if face.surface.as_plane().is_some() && polygons.len() == 1 + face.inner_loops.len() {
            validate_face_loop_arrangement(
                face_record.id,
                face,
                &polygons,
                linear_tolerance,
                diagnostics,
            );
        }
    }

    for shell in &topology.shells {
        for (face_index, face_key) in shell.value.faces.iter().enumerate() {
            if topology.face(*face_key).is_none() {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::DanglingEntityReference,
                    format!("shell/{}/face/{face_index}", shell.id.get()),
                ));
            }
        }
    }

    for solid in &topology.solids {
        if topology.shell(solid.value.outer_shell).is_none() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::DanglingEntityReference,
                format!("solid/{}/outer-shell", solid.id.get()),
            ));
        }
        for (index, inner) in solid.value.inner_shells.iter().enumerate() {
            if topology.shell(*inner).is_none() {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::DanglingEntityReference,
                    format!("solid/{}/inner-shell/{index}", solid.id.get()),
                ));
            }
        }
    }
}

fn validate_face_pcurves(
    topology: &Topology,
    face_id: EntityId,
    face: &Face,
    coedge_keys: &[CoedgeKey],
    loop_path: &str,
    linear_tolerance: f64,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (use_index, coedge_key) in coedge_keys.iter().enumerate() {
        let Some(coedge_record) = topology.coedge(*coedge_key) else {
            continue;
        };
        let Some(edge_record) = topology.edge(coedge_record.value.edge) else {
            continue;
        };
        let Some((_, curve_endpoints)) = topology.oriented_edge_vertices(&coedge_record.value)
        else {
            continue;
        };
        for (endpoint_index, curve_endpoint) in curve_endpoints.iter().enumerate() {
            let pcurve_endpoints = coedge_record.value.pcurve_endpoints();
            let surface_point = face.surface.evaluate(pcurve_endpoints[endpoint_index]);
            let distance = surface_point.distance(*curve_endpoint);
            if !distance.is_finite() || distance > linear_tolerance {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::PcurveEndpointMismatch,
                        format!(
                            "face/{}/{loop_path}/coedge/{use_index}/pcurve/{endpoint_index}",
                            face_id.get(),
                        ),
                    )
                    .with_measure(distance, linear_tolerance),
                );
            }
        }
        let locus_error = pcurve_locus_error(
            edge_record.value,
            coedge_record.value,
            face.surface,
            linear_tolerance,
        );
        if !locus_error.is_finite() || locus_error > linear_tolerance {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::PcurveLocusMismatch,
                    format!(
                        "face/{}/{loop_path}/coedge/{use_index}/pcurve/locus",
                        face_id.get(),
                    ),
                )
                .with_measure(locus_error, linear_tolerance),
            );
        }
    }
}

/// Certifies that a pcurve and its edge describe the same complete analytic
/// locus, not merely the same two endpoints. Each accepted pairing has an
/// invariant-specific proof: affine line/plane maps, fixed-angle cylinder
/// generators, or equal-frequency circular maps with coincident center,
/// initial radius, tangent, and effective frame normal.
fn pcurve_locus_error(
    edge: crate::topology::Edge,
    coedge: crate::topology::Coedge,
    surface: Surface,
    linear_tolerance: f64,
) -> f64 {
    let (edge_start, edge_end) = match coedge.orientation {
        Orientation::Forward => (edge.parameter_range.start, edge.parameter_range.end),
        Orientation::Reverse => (edge.parameter_range.end, edge.parameter_range.start),
    };
    let edge_delta = edge_end - edge_start;
    let pcurve_start = coedge.parameter_range.start;
    let pcurve_end = coedge.parameter_range.end;
    let pcurve_delta = pcurve_end - pcurve_start;

    let sampled_error = [0.25, 0.5, 0.75].into_iter().fold(0.0_f64, |worst, t| {
        let edge_parameter = edge_delta.mul_add(t, edge_start);
        let pcurve_parameter = pcurve_delta.mul_add(t, pcurve_start);
        worst.max(
            edge.curve
                .evaluate(edge_parameter)
                .distance(surface.evaluate(coedge.pcurve.evaluate(pcurve_parameter))),
        )
    });

    let edge_tangent = edge.curve.derivative(edge_start) * edge_delta;
    let pcurve_point = coedge.pcurve.evaluate(pcurve_start);
    let pcurve_derivative = coedge.pcurve.derivative(pcurve_start);
    let surface_tangent = surface.map_tangent(
        pcurve_point,
        crate::topology::Vector2::new(
            pcurve_derivative.x * pcurve_delta,
            pcurve_derivative.y * pcurve_delta,
        ),
    );
    let tangent_error = (edge_tangent - surface_tangent).length();

    let invariant_error = match (surface, coedge.pcurve, edge.curve) {
        (Surface::Plane(_), Curve2::Line { .. }, Curve3::Line { .. }) => 0.0,
        (
            Surface::Plane(plane),
            Curve2::Circle {
                center: pcenter,
                u: pu,
                v: pv,
                radius: pradius,
            },
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            },
        ) => {
            let mapped_center = plane.evaluate(pcenter);
            let mapped_u = plane.u * (pu.x * pradius) + plane.v * (pu.y * pradius);
            let mapped_v = plane.u * (pv.x * pradius) + plane.v * (pv.y * pradius);
            let mapped_normal = mapped_u.cross(mapped_v);
            let edge_normal = u.cross(v);
            mapped_center
                .distance(center)
                .max((pcurve_delta.abs() - edge_delta.abs()).abs() * radius.max(pradius))
                .max(effective_normal_error(
                    mapped_normal,
                    pcurve_delta,
                    edge_normal,
                    edge_delta,
                    radius.max(pradius),
                ))
        }
        (Surface::Cylinder(cylinder), Curve2::Line { endpoints }, Curve3::Line { .. }) => {
            let angular_motion = (endpoints[1].x - endpoints[0].x).abs() * cylinder.radius;
            angular_motion.max(tangent_error).max(sampled_error)
        }
        (
            Surface::Cylinder(cylinder),
            Curve2::Harmonic { .. },
            Curve3::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            },
        ) => {
            // The seam of two equal cylinders: every point of the ellipse
            // sits at the cylinder's radius from its axis, and its minor
            // radius is that radius. Eight samples around the whole ellipse
            // prove the locus; the sampled and tangent errors tie the
            // harmonic's parameterization to it.
            let axis_length = cylinder.axis.length();
            if axis_length <= f64::EPSILON {
                return linear_tolerance.max(1.0) * 2.0;
            }
            let axis = cylinder.axis / axis_length;
            let mut worst = (minor_radius - cylinder.radius).abs();
            for step in 0..8 {
                let t = f64::from(step) * std::f64::consts::FRAC_PI_4;
                let point = center + u * (major_radius * t.cos()) + v * (minor_radius * t.sin());
                let relative = point - cylinder.origin;
                let radial = relative - axis * relative.dot(axis);
                worst = worst.max((radial.length() - cylinder.radius).abs());
            }
            worst.max(tangent_error).max(sampled_error)
        }
        (
            Surface::Cylinder(cylinder),
            Curve2::Line { endpoints },
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            },
        ) => {
            let mapped_center = cylinder.origin + cylinder.axis * endpoints[0].y;
            let physical_sweep =
                cylinder.angular_sign * (endpoints[1].x - endpoints[0].x) * pcurve_delta;
            let axial_motion = (endpoints[1].y - endpoints[0].y).abs() * pcurve_delta.abs();
            mapped_center
                .distance(center)
                .max(axial_motion)
                .max((physical_sweep.abs() - edge_delta.abs()).abs() * radius)
                .max((radius - cylinder.radius).abs())
                .max(effective_normal_error(
                    cylinder.radial_u.cross(cylinder.radial_v),
                    physical_sweep,
                    u.cross(v),
                    edge_delta,
                    radius,
                ))
        }
        (
            Surface::Torus(torus),
            Curve2::Line { endpoints },
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            },
        ) => {
            let du = (endpoints[1].x - endpoints[0].x) * pcurve_delta;
            let dv = (endpoints[1].y - endpoints[0].y) * pcurve_delta;
            let iso_tolerance = 1.0e-12;
            if dv.abs() <= iso_tolerance {
                // Boundary ring at a fixed minor angle (wall or cap tangency).
                let minor_angle = endpoints[0].y;
                let ring_radius = torus.major_radius + torus.minor_radius * minor_angle.cos();
                let mapped_center =
                    torus.origin + torus.axis * (torus.minor_radius * minor_angle.sin());
                let physical_sweep = torus.angular_sign * du;
                mapped_center
                    .distance(center)
                    .max((radius - ring_radius).abs())
                    .max((physical_sweep.abs() - edge_delta.abs()).abs() * radius)
                    .max(effective_normal_error(
                        torus.radial_u.cross(torus.radial_v),
                        physical_sweep,
                        u.cross(v),
                        edge_delta,
                        radius,
                    ))
            } else if du.abs() <= iso_tolerance {
                // Seam generator: the minor circle at a fixed azimuth.
                let azimuth = torus.angular_sign * endpoints[0].x;
                let radial = torus.radial_u * azimuth.cos() + torus.radial_v * azimuth.sin();
                let mapped_center = torus.origin + radial * torus.major_radius;
                mapped_center
                    .distance(center)
                    .max((radius - torus.minor_radius).abs())
                    .max((dv.abs() - edge_delta.abs()).abs() * radius)
                    .max(effective_normal_error(
                        radial.cross(torus.axis),
                        dv,
                        u.cross(v),
                        edge_delta,
                        radius,
                    ))
            } else {
                f64::INFINITY
            }
        }
        (
            Surface::Cone(cone),
            Curve2::Line { endpoints },
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            },
        ) => {
            let du = (endpoints[1].x - endpoints[0].x) * pcurve_delta;
            let dv = (endpoints[1].y - endpoints[0].y) * pcurve_delta;
            if dv.abs() <= 1.0e-12 {
                // Boundary ring at a fixed axial offset.
                let axial = endpoints[0].y;
                let ring_radius = cone.ring_radius(axial);
                let mapped_center = cone.origin + cone.axis * axial;
                let physical_sweep = cone.angular_sign * du;
                mapped_center
                    .distance(center)
                    .max((radius - ring_radius).abs())
                    .max((physical_sweep.abs() - edge_delta.abs()).abs() * radius)
                    .max(effective_normal_error(
                        cone.radial_u.cross(cone.radial_v),
                        physical_sweep,
                        u.cross(v),
                        edge_delta,
                        radius,
                    ))
            } else {
                f64::INFINITY
            }
        }
        (Surface::Cone(cone), Curve2::Line { endpoints }, Curve3::Line { .. }) => {
            // Slant seam generator: the azimuth must stay fixed; the sampled
            // and tangent errors certify the line itself.
            let angular_motion = (endpoints[1].x - endpoints[0].x).abs()
                * cone
                    .ring_radius(endpoints[0].y)
                    .abs()
                    .max(cone.ring_radius(endpoints[1].y).abs());
            angular_motion.max(tangent_error).max(sampled_error)
        }
        (
            Surface::Sphere(sphere),
            Curve2::Line { endpoints },
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            },
        ) => {
            let du = (endpoints[1].x - endpoints[0].x) * pcurve_delta;
            let dv = (endpoints[1].y - endpoints[0].y) * pcurve_delta;
            let iso_tolerance = 1.0e-12;
            if dv.abs() <= iso_tolerance {
                // Latitude circle: centre offset along the axis, radius
                // shrunk by the cosine of the latitude.
                let latitude = endpoints[0].y;
                let mapped_center = sphere.origin + sphere.axis * (sphere.radius * latitude.sin());
                let mapped_radius = sphere.radius * latitude.cos();
                let physical_sweep = sphere.angular_sign * du;
                mapped_center
                    .distance(center)
                    .max((radius - mapped_radius).abs())
                    .max((physical_sweep.abs() - edge_delta.abs()).abs() * radius)
                    .max(effective_normal_error(
                        sphere.radial_u.cross(sphere.radial_v),
                        physical_sweep,
                        u.cross(v),
                        edge_delta,
                        radius,
                    ))
            } else if du.abs() <= iso_tolerance {
                // Meridian: a great circle through both poles.
                let azimuth = sphere.angular_sign * endpoints[0].x;
                let radial = sphere.radial_u * azimuth.cos() + sphere.radial_v * azimuth.sin();
                sphere
                    .origin
                    .distance(center)
                    .max((radius - sphere.radius).abs())
                    .max((dv.abs() - edge_delta.abs()).abs() * radius)
                    .max(effective_normal_error(
                        radial.cross(sphere.axis),
                        dv,
                        u.cross(v),
                        edge_delta,
                        radius,
                    ))
            } else {
                f64::INFINITY
            }
        }
        // Pole closure: a degenerate edge standing in for a parameter
        // singularity, where the surface's ring radius is exactly zero.
        (surface, Curve2::Line { endpoints }, Curve3::Line { endpoints: locus })
            if locus[0] == locus[1] =>
        {
            let ring_radius = match surface {
                Surface::Sphere(sphere) => (sphere.radius * endpoints[0].y.cos()).abs(),
                Surface::Cone(cone) => cone.ring_radius(endpoints[0].y).abs(),
                Surface::Torus(torus) => {
                    (torus.major_radius + torus.minor_radius * endpoints[0].y.cos()).abs()
                }
                Surface::Plane(_) | Surface::Cylinder(_) => return f64::INFINITY,
            };
            // Both pcurve endpoints must sit on the same singular iso-line.
            let iso = (endpoints[1].y - endpoints[0].y).abs();
            ring_radius.max(iso).max(sampled_error)
        }
        (
            Surface::Plane(plane),
            Curve2::Ellipse {
                center: pcenter,
                u: pu,
                v: pv,
                major_radius: pmajor,
                minor_radius: pminor,
            },
            Curve3::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            },
        ) => {
            // Same centre, same axes (mapped through the plane), same radii,
            // same parameter: an affine map of a circle is checked the way a
            // circle is, with the sampled and tangent errors tying the
            // parameterization.
            let mapped_center = plane.evaluate(pcenter);
            let mapped_u = plane.u * pu.x + plane.v * pu.y;
            let mapped_v = plane.u * pv.x + plane.v * pv.y;
            let scale = major_radius.max(pmajor);
            mapped_center
                .distance(center)
                .max((pmajor - major_radius).abs())
                .max((pminor - minor_radius).abs())
                .max((mapped_u - u).length() * scale)
                .max((mapped_v - v).length() * scale)
                .max((pcurve_delta - edge_delta).abs() * scale)
                .max(tangent_error)
                .max(sampled_error)
        }
        _ => f64::INFINITY,
    };

    let error = sampled_error.max(tangent_error).max(invariant_error);
    if error.is_finite() {
        error
    } else {
        // Keep diagnostics sortable and measurable even for an unsupported
        // analytic pairing or non-finite derived relation.
        linear_tolerance.max(1.0) * 2.0
    }
}

fn effective_normal_error(
    first: Vector3,
    first_sweep: f64,
    second: Vector3,
    second_sweep: f64,
    length_scale: f64,
) -> f64 {
    let first_length = first.length();
    let second_length = second.length();
    if first_length == 0.0 || second_length == 0.0 {
        return f64::INFINITY;
    }
    let alignment = (first / first_length).dot(second / second_length)
        * first_sweep.signum()
        * second_sweep.signum();
    (1.0 - alignment).abs() * length_scale
}

fn loop_parameter_area(topology: &Topology, loop_key: LoopKey) -> Option<f64> {
    let loop_record = topology.loop_record(loop_key)?;
    let mut area = 0.0;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology.coedge(*coedge_key)?.value;
        let [start, end] = coedge.pcurve_endpoints();
        let contribution = match coedge.pcurve {
            Curve2::Line { .. } => 0.5 * (start.x * end.y - start.y * end.x),
            Curve2::Circle {
                center,
                u,
                v,
                radius,
            } => {
                let sweep = coedge.parameter_range.end - coedge.parameter_range.start;
                let frame_determinant = u.x * v.y - u.y * v.x;
                0.5 * (center.x * (end.y - start.y) - center.y * (end.x - start.x)
                    + radius * radius * frame_determinant * sweep)
            }
            Curve2::Harmonic {
                mean,
                amplitude,
                phase,
            } => harmonic_area_contribution(
                mean,
                amplitude,
                phase,
                coedge.parameter_range.start,
                coedge.parameter_range.end,
            ),
            Curve2::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            } => {
                let sweep = coedge.parameter_range.end - coedge.parameter_range.start;
                let frame_determinant = u.x * v.y - u.y * v.x;
                0.5 * (center.x * (end.y - start.y) - center.y * (end.x - start.x)
                    + major_radius * minor_radius * frame_determinant * sweep)
            }
        };
        area += contribution;
    }
    area.is_finite().then_some(area)
}

/// `½∮(x dy − y dx)` along `(θ, m + A cos(θ − φ))` from `from` to `to`.
///
/// With `x = θ` and `y = m + A cos(θ − φ)`, the integrand is
/// `−Aθ sin(θ − φ) − m − A cos(θ − φ)`, whose antiderivative is
/// `Aθ cos(θ − φ) − 2A sin(θ − φ) − mθ`.
pub(crate) fn harmonic_area_contribution(
    mean: f64,
    amplitude: f64,
    phase: f64,
    from: f64,
    to: f64,
) -> f64 {
    let antiderivative = |t: f64| {
        let (sin, cos) = (t - phase).sin_cos();
        amplitude * t * cos - 2.0 * amplitude * sin - mean * t
    };
    0.5 * (antiderivative(to) - antiderivative(from))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointLocation {
    Outside,
    Boundary,
    Inside,
}

fn validate_face_loop_arrangement(
    face_id: EntityId,
    face: &Face,
    polygons: &[Vec<Point3>],
    linear_tolerance: f64,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let plane = face
        .surface
        .as_plane()
        .expect("linear loop arrangement is only requested on planar faces");
    let projected = polygons
        .iter()
        .map(|polygon| {
            polygon
                .iter()
                .map(|point| plane.project(*point))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    for (loop_index, polygon) in projected.iter().enumerate() {
        if polygon_self_intersects(polygon, linear_tolerance) {
            let loop_path = if loop_index == 0 {
                "outer-loop".to_owned()
            } else {
                format!("inner-loop/{}", loop_index - 1)
            };
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::FaceLoopIntersection,
                format!("face/{}/{loop_path}/self-intersection", face_id.get()),
            ));
        }
    }

    let outer = &projected[0];
    for (inner_index, inner) in projected[1..].iter().enumerate() {
        if polygons_intersect(outer, inner, linear_tolerance) {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::FaceLoopIntersection,
                format!("face/{}/inner-loop/{inner_index}/outer", face_id.get()),
            ));
        }
        if inner.first().is_none_or(|point| {
            point_location(*point, outer, linear_tolerance) != PointLocation::Inside
        }) {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::FaceHoleOutside,
                format!(
                    "face/{}/inner-loop/{inner_index}/containment",
                    face_id.get()
                ),
            ));
        }
    }

    for left in 1..projected.len() {
        for right in left + 1..projected.len() {
            let left_polygon = &projected[left];
            let right_polygon = &projected[right];
            let intersects = polygons_intersect(left_polygon, right_polygon, linear_tolerance);
            let nested = left_polygon.first().is_some_and(|point| {
                point_location(*point, right_polygon, linear_tolerance) != PointLocation::Outside
            }) || right_polygon.first().is_some_and(|point| {
                point_location(*point, left_polygon, linear_tolerance) != PointLocation::Outside
            });
            if intersects || nested {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::FaceLoopIntersection,
                    format!(
                        "face/{}/inner-loop/{}/inner-loop/{}",
                        face_id.get(),
                        left - 1,
                        right - 1
                    ),
                ));
            }
        }
    }
}

fn polygon_self_intersects(polygon: &[Point2], tolerance: f64) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    for first in 0..polygon.len() {
        let first_next = (first + 1) % polygon.len();
        for second in first + 1..polygon.len() {
            let second_next = (second + 1) % polygon.len();
            if first == second
                || first == second_next
                || first_next == second
                || first_next == second_next
            {
                continue;
            }
            if segments_intersect_or_touch(
                polygon[first],
                polygon[first_next],
                polygon[second],
                polygon[second_next],
                tolerance,
            ) {
                return true;
            }
        }
    }
    false
}

fn polygons_intersect(left: &[Point2], right: &[Point2], tolerance: f64) -> bool {
    for left_index in 0..left.len() {
        let left_next = (left_index + 1) % left.len();
        for right_index in 0..right.len() {
            let right_next = (right_index + 1) % right.len();
            if segments_intersect_or_touch(
                left[left_index],
                left[left_next],
                right[right_index],
                right[right_next],
                tolerance,
            ) {
                return true;
            }
        }
    }
    false
}

fn segments_intersect_or_touch(
    first_start: Point2,
    first_end: Point2,
    second_start: Point2,
    second_end: Point2,
    tolerance: f64,
) -> bool {
    if first_start.x.min(first_end.x) - tolerance > second_start.x.max(second_end.x)
        || second_start.x.min(second_end.x) - tolerance > first_start.x.max(first_end.x)
        || first_start.y.min(first_end.y) - tolerance > second_start.y.max(second_end.y)
        || second_start.y.min(second_end.y) - tolerance > first_start.y.max(first_end.y)
    {
        return false;
    }
    if point_segment_distance(first_start, second_start, second_end) <= tolerance
        || point_segment_distance(first_end, second_start, second_end) <= tolerance
        || point_segment_distance(second_start, first_start, first_end) <= tolerance
        || point_segment_distance(second_end, first_start, first_end) <= tolerance
    {
        return true;
    }
    let orientations = [
        cross_2d(first_start, first_end, second_start),
        cross_2d(first_start, first_end, second_end),
        cross_2d(second_start, second_end, first_start),
        cross_2d(second_start, second_end, first_end),
    ];
    orientations[0].is_sign_positive() != orientations[1].is_sign_positive()
        && orientations[2].is_sign_positive() != orientations[3].is_sign_positive()
}

fn point_location(point: Point2, polygon: &[Point2], tolerance: f64) -> PointLocation {
    let mut inside = false;
    for index in 0..polygon.len() {
        let start = polygon[index];
        let end = polygon[(index + 1) % polygon.len()];
        if point_segment_distance(point, start, end) <= tolerance {
            return PointLocation::Boundary;
        }
        if (start.y > point.y) != (end.y > point.y) {
            let intersection_x =
                (end.x - start.x) * (point.y - start.y) / (end.y - start.y) + start.x;
            if point.x < intersection_x {
                inside = !inside;
            }
        }
    }
    if inside {
        PointLocation::Inside
    } else {
        PointLocation::Outside
    }
}

fn point_segment_distance(point: Point2, start: Point2, end: Point2) -> f64 {
    let segment = Point2::new(end.x - start.x, end.y - start.y);
    let length_squared = segment.x.mul_add(segment.x, segment.y * segment.y);
    if length_squared <= 0.0 || !length_squared.is_finite() {
        return f64::INFINITY;
    }
    let projection =
        ((point.x - start.x) * segment.x + (point.y - start.y) * segment.y) / length_squared;
    let parameter = projection.clamp(0.0, 1.0);
    (point.x - (start.x + parameter * segment.x)).hypot(point.y - (start.y + parameter * segment.y))
}

fn cross_2d(origin: Point2, first: Point2, second: Point2) -> f64 {
    (first.x - origin.x) * (second.y - origin.y) - (first.y - origin.y) * (second.x - origin.x)
}

fn validate_edge_uses(topology: &Topology, diagnostics: &mut Vec<Diagnostic>) {
    let mut uses = BTreeMap::<EdgeKey, Vec<(EntityId, Orientation)>>::new();
    for coedge in &topology.coedges {
        if topology.edge(coedge.value.edge).is_some() {
            uses.entry(coedge.value.edge)
                .or_default()
                .push((coedge.id, coedge.value.orientation));
        }
    }

    for (edge_index, edge) in topology.edges.iter().enumerate() {
        let edge_uses = uses
            .get(&EdgeKey(edge_index))
            .map_or(&[][..], Vec::as_slice);
        if edge_uses.len() != 2 {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::EdgeUseCount,
                    format!("edge/{}/uses", edge.id.get()),
                )
                .with_measure(edge_uses.len() as f64, 2.0),
            );
            continue;
        }
        if edge_uses[0].1 != edge_uses[1].1.reversed() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::EdgeUseOrientation,
                format!("edge/{}/uses", edge.id.get()),
            ));
        }
    }
}

fn validate_ownership_and_shells(topology: &Topology, diagnostics: &mut Vec<Diagnostic>) {
    let mut coedge_uses = vec![0_usize; topology.coedges.len()];
    for loop_record in &topology.loops {
        for key in &loop_record.value.coedges {
            if let Some(count) = coedge_uses.get_mut(key.0) {
                *count += 1;
            }
        }
    }
    diagnose_use_counts(
        &topology
            .coedges
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        &coedge_uses,
        DiagnosticCode::CoedgeUseCount,
        "coedge",
        diagnostics,
    );

    let mut loop_uses = vec![0_usize; topology.loops.len()];
    for face in &topology.faces {
        for loop_key in face.value.loops() {
            if let Some(count) = loop_uses.get_mut(loop_key.0) {
                *count += 1;
            }
        }
    }
    diagnose_use_counts(
        &topology
            .loops
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        &loop_uses,
        DiagnosticCode::LoopUseCount,
        "loop",
        diagnostics,
    );

    let mut face_uses = vec![0_usize; topology.faces.len()];
    for shell in &topology.shells {
        for key in &shell.value.faces {
            if let Some(count) = face_uses.get_mut(key.0) {
                *count += 1;
            }
        }
    }
    diagnose_use_counts(
        &topology
            .faces
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        &face_uses,
        DiagnosticCode::FaceUseCount,
        "face",
        diagnostics,
    );

    let mut shell_uses = vec![0_usize; topology.shells.len()];
    for solid in &topology.solids {
        for shell in solid.value.shells() {
            if let Some(count) = shell_uses.get_mut(shell.0) {
                *count += 1;
            }
        }
    }
    diagnose_use_counts(
        &topology
            .shells
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        &shell_uses,
        DiagnosticCode::ShellUseCount,
        "shell",
        diagnostics,
    );

    for shell in &topology.shells {
        validate_shell(topology, shell.id, &shell.value.faces, diagnostics);
    }
}

fn diagnose_use_counts(
    ids: &[EntityId],
    counts: &[usize],
    code: DiagnosticCode,
    kind: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (id, count) in ids.iter().zip(counts) {
        if *count != 1 {
            diagnostics.push(
                Diagnostic::new(code, format!("{kind}/{}/owner-uses", id.get()))
                    .with_measure(*count as f64, 1.0),
            );
        }
    }
}

fn validate_shell(
    topology: &Topology,
    shell_id: EntityId,
    face_keys: &[FaceKey],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let faces = face_keys
        .iter()
        .copied()
        .filter(|key| topology.face(*key).is_some())
        .collect::<BTreeSet<_>>();
    if faces.is_empty() {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::ShellDisconnected,
            format!("shell/{}/connectivity", shell_id.get()),
        ));
        return;
    }

    let mut face_edges = BTreeMap::<FaceKey, BTreeSet<EdgeKey>>::new();
    let mut shell_edges = BTreeSet::new();
    let mut shell_vertices = BTreeSet::new();
    for face_key in &faces {
        let Some(face) = topology.face(*face_key) else {
            continue;
        };
        let edges = face_edges.entry(*face_key).or_default();
        for loop_key in face.value.loops() {
            let Some(loop_record) = topology.loop_record(loop_key) else {
                continue;
            };
            for coedge_key in &loop_record.value.coedges {
                let Some(coedge) = topology.coedge(*coedge_key) else {
                    continue;
                };
                edges.insert(coedge.value.edge);
                shell_edges.insert(coedge.value.edge);
                if let Some(edge) = topology.edge(coedge.value.edge) {
                    shell_vertices.extend(edge.value.vertices);
                }
            }
        }
    }

    let face_cells = faces
        .iter()
        .filter_map(|face_key| topology.face(*face_key))
        .map(|face| 1_i64 - face.value.inner_loops.len() as i64)
        .sum::<i64>();
    // A pole edge is a parameter-space device, not a topological 1-cell: the
    // point set it stands for is the single vertex at its two ends, already
    // counted. Including it would subtract an edge the surface does not have
    // and drive a sphere's characteristic to one.
    let degenerate_edges = shell_edges
        .iter()
        .filter(|edge_key| {
            topology
                .edge(**edge_key)
                .is_some_and(|edge| edge.value.vertices[0] == edge.value.vertices[1])
        })
        .count() as i64;
    let euler =
        shell_vertices.len() as i64 - (shell_edges.len() as i64 - degenerate_edges) + face_cells;
    // A connected, closed, orientable shell has χ = 2 - 2g. Earlier slices
    // accepted only genus zero (χ = 2), which incorrectly rejected an exact
    // through-hole (genus one, χ = 0) despite manifold edge use and positive
    // volume. Odd values and values above two cannot describe such a shell.
    if euler > 2 || euler.rem_euclid(2) != 0 {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::EulerCharacteristicInvalid,
                format!("shell/{}/euler", shell_id.get()),
            )
            .with_measure(euler as f64, 2.0),
        );
    }

    let start = *faces.first().expect("non-empty checked above");
    let mut visited = BTreeSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(current) = queue.pop_front() {
        let Some(current_edges) = face_edges.get(&current) else {
            continue;
        };
        for candidate in &faces {
            if !visited.contains(candidate)
                && face_edges
                    .get(candidate)
                    .is_some_and(|edges| !current_edges.is_disjoint(edges))
            {
                visited.insert(*candidate);
                queue.push_back(*candidate);
            }
        }
    }
    if visited.len() != faces.len() {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::ShellDisconnected,
                format!("shell/{}/connectivity", shell_id.get()),
            )
            .with_measure(visited.len() as f64, faces.len() as f64),
        );
    }

    // Opposite coedge uses certify a consistently oriented closed manifold;
    // the positive signed-volume check in `validate` then rejects a globally
    // inverted shell. A face-center dot arithmetic-shell-center heuristic is
    // intentionally not used: it is valid only for star-shaped/convex solids
    // and incorrectly rejects legitimate pocket walls on concave bodies.
}

pub(crate) fn calculate_measures(topology: &Topology) -> ShapeMeasures {
    let bounds = calculate_bounds(topology);
    // Any curved face routes to the exact shell engine: one set of per-class
    // closed forms covers every surface in the vocabulary at any orientation
    // and yields area, volume, and centroid alike. The prismatic strategies
    // below must not see curved solids at all — their profile-times-height
    // shortcut silently mismeasures anything that is not a pure extrusion,
    // such as a blind pocket. A domain violation yields zero measures, which
    // the solid-volume family then rejects fail-closed.
    if topology
        .faces
        .iter()
        .any(|face| !matches!(face.value.surface, Surface::Plane(_)))
    {
        return calculate_exact_shell_measures(topology, bounds).unwrap_or(ShapeMeasures {
            bounds,
            surface_area: 0.0,
            signed_volume: 0.0,
            centroid: None,
        });
    }
    let has_feature_sides = topology.shells.iter().any(|shell| {
        shell.value.faces.iter().any(|key| {
            topology.face(*key).is_some_and(|face| {
                matches!(face.value.role, crate::topology::FaceRole::FeatureSide(_))
            })
        })
    });
    let has_analytic_curves = topology
        .edges
        .iter()
        .any(|edge| matches!(edge.value.curve, Curve3::Circle { .. }));
    // `FeatureSide` identifies a selected-face feature, while the analytic
    // curve is what requires the exact measure reconstruction. Its feature
    // wall may itself be planar (a polygon on a circular owner) or cylindrical
    // (an arc/circle). Check this combination before the generic analytic-
    // extrusion fallback: that fallback understands the circular owner but
    // not its imprinted boss or pocket. An all-linear legacy face feature has
    // no analytic curve and remains on its established polyhedral path.
    if has_feature_sides
        && has_analytic_curves
        && let Some(measures) = calculate_analytic_face_feature_measures(topology, bounds)
    {
        return measures;
    }
    if has_analytic_curves
        && let Some(measures) = calculate_analytic_extrusion_measures(topology, bounds)
    {
        return measures;
    }
    // Anchor volume integration near the body instead of at the world origin.
    // This avoids catastrophic cancellation for a small solid translated to a
    // large (but still supported) world coordinate.
    let reference = bounds.map_or_else(Point3::default, |bounds| {
        Point3::new(
            bounds.min.x + (bounds.max.x - bounds.min.x) * 0.5,
            bounds.min.y + (bounds.max.y - bounds.min.y) * 0.5,
            bounds.min.z + (bounds.max.z - bounds.min.z) * 0.5,
        )
    });
    let mut surface_area = 0.0;
    let mut signed_volume = 0.0;
    let mut centroid_numerator = Vector3::default();

    let mut visited_faces = BTreeMap::<FaceKey, ()>::new();
    let shell_faces = |solid: &crate::topology::Solid| {
        solid
            .shells()
            .filter_map(|key| topology.shell(key))
            .flat_map(|shell| shell.value.faces.clone())
            .collect::<Vec<_>>()
    };
    for solid_index in 0..topology.solids.len() {
        let Some(solid) = topology.solid(SolidKey(solid_index)) else {
            continue;
        };
        for face_key in &shell_faces(&solid.value) {
            if visited_faces.insert(*face_key, ()).is_some() {
                continue;
            }
            let Some(face) = topology.face(*face_key) else {
                continue;
            };
            let Some(polygons) = face_boundary_polygons(topology, &face.value) else {
                continue;
            };
            let face_area_vector = polygons.iter().fold(Vector3::default(), |area, polygon| {
                area + polygon_area_vector(polygon)
            });
            surface_area += face_area_vector.length();

            // Loop windings make this a signed boundary integral: inner-loop
            // fans subtract their void from the outer fan without introducing
            // display tessellation into authoritative measures.
            for polygon in polygons {
                if polygon.len() < 3 {
                    continue;
                }
                let anchor = polygon[0];
                for index in 1..polygon.len() - 1 {
                    let b = polygon[index];
                    let c = polygon[index + 1];
                    let tetrahedron_volume =
                        (anchor - reference).dot((b - reference).cross(c - reference)) / 6.0;
                    signed_volume += tetrahedron_volume;
                    centroid_numerator = centroid_numerator
                        + (reference.as_vector()
                            + anchor.as_vector()
                            + b.as_vector()
                            + c.as_vector())
                            * (tetrahedron_volume / 4.0);
                }
            }
        }
    }

    let centroid = if signed_volume.is_finite() && signed_volume.abs() > 0.0 {
        let vector = centroid_numerator / signed_volume;
        Some(Point3::new(vector.x, vector.y, vector.z))
    } else {
        None
    };

    ShapeMeasures {
        bounds,
        surface_area,
        signed_volume,
        centroid,
    }
}

/// Exact area and volume for any shell of planes, cylinders, and spheres,
/// with no common axis required.
///
/// The divergence theorem turns the volume into a boundary integral,
/// `3V = ∮ (x − p)·n dA`, and each surface class contributes a closed form
/// over its own parameter domain, so no tolerance and no sampling enters.
///
/// The centroid follows from a second identity of the same kind. Because
/// `div(|y|²/2 · e) = y·e` for any fixed direction `e`, the first moment is
/// `V·(c − p) = ∮ (|y|²/2) n dA` with `y = x − p`; one vector integral per
/// face yields all three coordinates. Every surface here is a surface of
/// revolution about its own axis, `P = origin + ρ(v)·radial(s·u) + z(v)·axis`,
/// whose unnormalized normal is `s·ρ·(z'·radial − ρ'·axis)`, so the whole
/// family shares one assembly and differs only in four scalar integrals of
/// `ρ` and `z` over the face's own latitude range.
pub(crate) fn calculate_exact_shell_measures(
    topology: &Topology,
    bounds: Option<Bounds3>,
) -> Option<ShapeMeasures> {
    // Anchor near the body so a distant solid keeps its precision.
    let anchor = bounds.map_or_else(Point3::default, |bounds| {
        Point3::new(
            bounds.min.x + (bounds.max.x - bounds.min.x) * 0.5,
            bounds.min.y + (bounds.max.y - bounds.min.y) * 0.5,
            bounds.min.z + (bounds.max.z - bounds.min.z) * 0.5,
        )
    });
    let mut surface_area = 0.0;
    let mut flux = 0.0;
    let mut moment = Vector3::new(0.0, 0.0, 0.0);

    for face in &topology.faces {
        // A face whose loop winds clockwise in parameter space has its outward
        // normal opposite to the surface's own, so its flux changes sign. The
        // area never does.
        let (parameter_area, _) = face_parameter_area_and_moment(topology, &face.value)?;
        let orientation = if parameter_area < 0.0 { -1.0 } else { 1.0 };
        match face.value.surface {
            Surface::Plane(plane) => {
                // The parameter area is the true area for an orthonormal
                // planar frame, and the arm below rejects any other frame.
                let jacobian = plane.u.cross(plane.v).length();
                if (jacobian - 1.0).abs() > 1.0e-9 {
                    return None;
                }
                if (plane.u.length() - 1.0).abs() > 1.0e-9
                    || (plane.v.length() - 1.0).abs() > 1.0e-9
                    || plane.u.dot(plane.v).abs() > 1.0e-9
                {
                    return None;
                }
                let offset = (plane.origin - anchor).dot(plane.normal);
                surface_area += parameter_area.abs();
                flux += offset * parameter_area;
                // ∫∫|y|² over the region, expanded about the parameter anchor
                // the loop helpers share: |y|² = |c|² + 2c·(u'U + v'V) + |w'|²
                // for an orthonormal planar frame.
                let (_, first) = face_parameter_area_and_moment(topology, &face.value)?;
                let polar = face_parameter_polar_moment(topology, &face.value)?;
                let base = plane.origin - anchor;
                let square = base.dot(base).mul_add(
                    parameter_area,
                    2.0 * base
                        .dot(plane.u)
                        .mul_add(first.x, base.dot(plane.v) * first.y),
                ) + polar;
                moment = moment + plane.normal * (square / 2.0);
            }
            Surface::Cylinder(cylinder)
                if let Some(contour) =
                    cylinder_contour_measures(topology, &face.value, cylinder, anchor) =>
            {
                // The general path: the parameter region bounded by any
                // mix of axis-aligned lines and harmonics, integrated by
                // Green's theorem along the loops. A rectangle takes the
                // arm below, which it must agree with to the last bit.
                surface_area += contour.area.abs();
                flux += contour.flux;
                moment = moment + contour.moment;
            }
            Surface::Cylinder(cylinder) => {
                let (u_min, u_max, v_min, v_max) = pcurve_extent(topology, &face.value)?;
                let sign = cylinder.angular_sign;
                let radial_integral =
                    cylinder.radial_u * ((sign * u_max).sin() - (sign * u_min).sin()) / sign
                        + cylinder.radial_v * ((sign * u_min).cos() - (sign * u_max).cos()) / sign;
                let sweep = u_max - u_min;
                let extent = v_max - v_min;
                let area = cylinder.radius * sweep * extent;
                surface_area += area.abs();
                // (x − p)·n = (origin − p)·radial(u) + radius.
                flux += orientation
                    * cylinder.angular_sign
                    * cylinder.radius
                    * ((cylinder.origin - anchor).dot(radial_integral) * extent
                        + cylinder.radius * sweep * extent);

                // ρ = r, z = v: the latitude integrals collapse to powers of v.
                let azimuth = azimuth_moments(
                    cylinder.radial_u,
                    cylinder.radial_v,
                    sign,
                    cylinder.origin - anchor,
                    u_min,
                    u_max,
                );
                let offset = cylinder.origin - anchor;
                let axial = offset.dot(cylinder.axis);
                let first = v_max.mul_add(v_max, -(v_min * v_min)) / 2.0;
                let second = (v_max.powi(3) - v_min.powi(3)) / 3.0;
                let radius = cylinder.radius;
                let intrinsic = offset.dot(offset) + radius * radius;
                let along = radius * (intrinsic.mul_add(extent, second) + 2.0 * axial * first);
                let across = radius * radius * extent;
                moment = moment
                    + revolution_moment(
                        orientation * sign,
                        &azimuth,
                        cylinder.axis,
                        [along, across, 0.0, 0.0],
                    );
            }
            Surface::Sphere(sphere) => {
                let (u_min, u_max, v_min, v_max) = pcurve_extent(topology, &face.value)?;
                let sign = sphere.angular_sign;
                let radial_integral =
                    sphere.radial_u * ((sign * u_max).sin() - (sign * u_min).sin()) / sign
                        + sphere.radial_v * ((sign * u_min).cos() - (sign * u_max).cos()) / sign;
                let sweep = u_max - u_min;
                let sine_span = v_max.sin() - v_min.sin();
                let area = sphere.radius * sphere.radius * sweep * sine_span;
                surface_area += area.abs();
                // ∮ n cos v du dv splits into the radial and axial halves.
                let cosine_square = ((v_max + v_max.sin() * v_max.cos())
                    - (v_min + v_min.sin() * v_min.cos()))
                    / 2.0;
                let sine_cosine = (v_max.sin().powi(2) - v_min.sin().powi(2)) / 2.0;
                let normal_integral =
                    radial_integral * cosine_square + sphere.axis * (sweep * sine_cosine);
                flux += orientation
                    * sphere.angular_sign
                    * sphere.radius
                    * sphere.radius
                    * ((sphere.origin - anchor).dot(normal_integral)
                        + sphere.radius * sweep * sine_span);

                // ρ = r cos v, z = r sin v, so ρ² + z² is the constant r².
                let azimuth = azimuth_moments(
                    sphere.radial_u,
                    sphere.radial_v,
                    sign,
                    sphere.origin - anchor,
                    u_min,
                    u_max,
                );
                let offset = sphere.origin - anchor;
                let axial = offset.dot(sphere.axis);
                let radius = sphere.radius;
                let constant = offset.dot(offset) + radius * radius;
                let twist = 2.0 * radius * axial;
                let along = radius
                    * radius
                    * constant.mul_add(
                        trig_moment(2, 0, v_min, v_max),
                        twist * trig_moment(2, 1, v_min, v_max),
                    );
                let across = radius.powi(3) * trig_moment(3, 0, v_min, v_max);
                let axial_along = -radius
                    * radius
                    * constant.mul_add(
                        trig_moment(1, 1, v_min, v_max),
                        twist * trig_moment(1, 2, v_min, v_max),
                    );
                let axial_across = -radius.powi(3) * trig_moment(2, 1, v_min, v_max);
                moment = moment
                    + revolution_moment(
                        orientation * sign,
                        &azimuth,
                        sphere.axis,
                        [along, across, axial_along, axial_across],
                    );
            }
            Surface::Torus(torus) => {
                let (u_min, u_max, v_min, v_max) = pcurve_extent(topology, &face.value)?;
                let sign = torus.angular_sign;
                let radial_integral =
                    torus.radial_u * ((sign * u_max).sin() - (sign * u_min).sin()) / sign
                        + torus.radial_v * ((sign * u_min).cos() - (sign * u_max).cos()) / sign;
                let sweep = u_max - u_min;
                let major = torus.major_radius;
                let minor = torus.minor_radius;
                // Minor-angle integrals shared by the area and the flux.
                let sine = v_max.sin() - v_min.sin();
                let cosine = v_min.cos() - v_max.cos();
                let cosine_square = ((v_max + v_max.sin() * v_max.cos())
                    - (v_min + v_min.sin() * v_min.cos()))
                    / 2.0;
                let sine_cosine = (v_max.sin().powi(2) - v_min.sin().powi(2)) / 2.0;
                let span = v_max - v_min;

                let area = minor * sweep * major.mul_add(span, minor * sine);
                surface_area += area.abs();
                // (x - p).n = (origin - p).n + major*cos v + minor, and
                // dA = minor*(major + minor*cos v) du dv.
                let normal_integral = radial_integral * major.mul_add(sine, minor * cosine_square)
                    + torus.axis * (sweep * major.mul_add(cosine, minor * sine_cosine));
                let intrinsic = minor
                    * sweep
                    * (major * major * sine
                        + major * minor * cosine_square
                        + minor * major * span
                        + minor * minor * sine);
                flux += orientation
                    * sign
                    * (minor * (torus.origin - anchor).dot(normal_integral) + intrinsic);

                // ρ = R + r cos v, z = r sin v, so ρ² + z² = R² + r² + 2Rr cos v.
                let azimuth = azimuth_moments(
                    torus.radial_u,
                    torus.radial_v,
                    sign,
                    torus.origin - anchor,
                    u_min,
                    u_max,
                );
                let offset = torus.origin - anchor;
                let axial = offset.dot(torus.axis);
                let constant = offset.dot(offset) + major * major + minor * minor;
                let cosine_weight = 2.0 * major * minor;
                let sine_weight = 2.0 * minor * axial;
                let moments = |cosines: u32, sines: u32| trig_moment(cosines, sines, v_min, v_max);
                let along = minor
                    * (major * constant * moments(1, 0)
                        + 2.0 * major * major * minor * moments(2, 0)
                        + major * sine_weight * moments(1, 1)
                        + minor * constant * moments(2, 0)
                        + minor * cosine_weight * moments(3, 0)
                        + minor * sine_weight * moments(2, 1));
                let across = minor
                    * (major * major * moments(1, 0)
                        + cosine_weight * moments(2, 0)
                        + minor * minor * moments(3, 0));
                let axial_along = -minor
                    * (major * constant * moments(0, 1)
                        + 2.0 * major * major * minor * moments(1, 1)
                        + major * sine_weight * moments(0, 2)
                        + minor * constant * moments(1, 1)
                        + minor * cosine_weight * moments(2, 1)
                        + minor * sine_weight * moments(1, 2));
                let axial_across = -minor
                    * (major * major * moments(0, 1)
                        + cosine_weight * moments(1, 1)
                        + minor * minor * moments(2, 1));
                moment = moment
                    + revolution_moment(
                        orientation * sign,
                        &azimuth,
                        torus.axis,
                        [along, across, axial_along, axial_across],
                    );
            }
            Surface::Cone(cone) => {
                let (u_min, u_max, v_min, v_max) = pcurve_extent(topology, &face.value)?;
                let sign = cone.angular_sign;
                let radial_integral = cone.radial_u * ((sign * u_max).sin() - (sign * u_min).sin())
                    / sign
                    + cone.radial_v * ((sign * u_min).cos() - (sign * u_max).cos()) / sign;
                let sweep = u_max - u_min;
                let base = cone.base_radius;
                let slope = cone.slope;
                // Ring-radius moments over the face's own axial extent.
                let span = v_max - v_min;
                let square = v_max.mul_add(v_max, -(v_min * v_min)) / 2.0;
                let cube = v_max.powi(3) - v_min.powi(3);
                let ring = slope.mul_add(square, base * span);
                let ring_square = base.mul_add(
                    base * span,
                    (base * slope).mul_add(2.0 * square, slope * slope * cube / 3.0),
                );
                let ring_moment = base.mul_add(square, slope * cube / 3.0);
                // dA = ring(v)·sqrt(1 + slope²) du dv, and the unit normal is
                // (radial − slope·axis)/sqrt(1 + slope²), so the root cancels
                // out of the flux entirely.
                surface_area += (slope.mul_add(slope, 1.0).sqrt() * sweep * ring).abs();
                let axial = (cone.origin - anchor).dot(cone.axis);
                flux += orientation
                    * sign
                    * ((cone.origin - anchor).dot(radial_integral) * ring
                        + sweep * slope.mul_add(-axial.mul_add(ring, ring_moment), ring_square));

                // ρ = b + m·v, z = v: every latitude integral is polynomial,
                // and ρ' = m makes the axial pair a multiple of the radial one.
                let azimuth = azimuth_moments(
                    cone.radial_u,
                    cone.radial_v,
                    sign,
                    cone.origin - anchor,
                    u_min,
                    u_max,
                );
                let offset = cone.origin - anchor;
                let first = v_max.mul_add(v_max, -(v_min * v_min)) / 2.0;
                let second = (v_max.powi(3) - v_min.powi(3)) / 3.0;
                let third = (v_max.powi(4) - v_min.powi(4)) / 4.0;
                let cube = base.powi(3) * span
                    + 3.0 * base * base * slope * first
                    + 3.0 * base * slope * slope * second
                    + slope.powi(3) * third;
                let along = offset.dot(offset).mul_add(
                    slope.mul_add(first, base * span),
                    cube + slope.mul_add(third, base * second),
                ) + 2.0 * axial * slope.mul_add(second, base * first);
                let across = base.mul_add(
                    base * span,
                    slope.mul_add(2.0 * base * first, slope * slope * second),
                );
                moment = moment
                    + revolution_moment(
                        orientation * sign,
                        &azimuth,
                        cone.axis,
                        [along, across, slope * along, slope * across],
                    );
            }
        }
    }

    let signed_volume = flux / 3.0;
    let centroid = (signed_volume.abs() > 0.0 && moment.is_finite()).then(|| {
        Point3::new(
            anchor.x + moment.x / signed_volume,
            anchor.y + moment.y / signed_volume,
            anchor.z + moment.z / signed_volume,
        )
    });
    (surface_area.is_finite() && signed_volume.is_finite() && signed_volume > 0.0).then_some(
        ShapeMeasures {
            bounds,
            surface_area,
            signed_volume,
            centroid,
        },
    )
}

/// The azimuth integrals every surface of revolution in the vocabulary needs.
struct AzimuthMoments {
    /// `∫ du` over the face's azimuth range.
    sweep: f64,
    /// `∫ radial(s·u) du`.
    radial: Vector3,
    /// `∫ radial(s·u)·(offset·radial(s·u)) du`.
    radial_offset: Vector3,
    /// `∫ offset·radial(s·u) du`.
    offset_radial: f64,
}

/// A polynomial in `cos θ` and `sin θ`, as terms `(coefficient, cosines,
/// sines)`. Products of the surface integrands and the powers of a harmonic
/// boundary all live here, and integrate term by term.
#[derive(Clone, Debug)]
struct TrigPoly(Vec<(f64, u32, u32)>);

impl TrigPoly {
    fn constant(value: f64) -> Self {
        Self(vec![(value, 0, 0)])
    }

    fn cosine() -> Self {
        Self(vec![(1.0, 1, 0)])
    }

    fn sine() -> Self {
        Self(vec![(1.0, 0, 1)])
    }

    fn scaled(&self, factor: f64) -> Self {
        Self(
            self.0
                .iter()
                .map(|(coefficient, cosines, sines)| (coefficient * factor, *cosines, *sines))
                .collect(),
        )
    }

    fn plus(&self, other: &Self) -> Self {
        let mut terms = self.0.clone();
        terms.extend(other.0.iter().copied());
        Self(terms)
    }

    fn times(&self, other: &Self) -> Self {
        let mut terms = Vec::with_capacity(self.0.len() * other.0.len());
        for (left, left_cos, left_sin) in &self.0 {
            for (right, right_cos, right_sin) in &other.0 {
                terms.push((left * right, left_cos + right_cos, left_sin + right_sin));
            }
        }
        Self(terms)
    }

    fn power(&self, exponent: u32) -> Self {
        let mut result = Self::constant(1.0);
        for _ in 0..exponent {
            result = result.times(self);
        }
        result
    }

    fn integrate(&self, from: f64, to: f64) -> f64 {
        self.0
            .iter()
            .map(|(coefficient, cosines, sines)| {
                coefficient * trig_power_integral(*cosines, *sines, from, to)
            })
            .sum()
    }
}

/// `∫ cos^a t sin^b t dt` for any powers, by the classical reduction
/// formulas applied to the antiderivative: closed form, no quadrature.
fn trig_power_integral(cosines: u32, sines: u32, from: f64, to: f64) -> f64 {
    fn antiderivative(a: u32, b: u32, t: f64) -> f64 {
        let (sin, cos) = t.sin_cos();
        let total = f64::from(a + b);
        match (a, b) {
            (0, 0) => t,
            (1, 0) => sin,
            (0, 1) => -cos,
            (1, 1) => sin * sin / 2.0,
            (_, b) if b >= 2 => {
                -cos.powi(a as i32 + 1) * sin.powi(b as i32 - 1) / total
                    + f64::from(b - 1) / total * antiderivative(a, b - 2, t)
            }
            _ => {
                cos.powi(a as i32 - 1) * sin.powi(b as i32 + 1) / total
                    + f64::from(a - 1) / total * antiderivative(a - 2, b, t)
            }
        }
    }
    antiderivative(cosines, sines, to) - antiderivative(cosines, sines, from)
}

/// `∬_D t(θ)·v^k dθ dv` over a cylinder face's parameter region, by Green's
/// theorem as `−∮ t(θ)·v^{k+1}/(k+1) dθ` along its loops. The loops' own
/// winding signs the result, so a clockwise face comes out negative exactly
/// as its parameter area does. Only axis-aligned lines and harmonics bound
/// the regions this closes; a sloped line returns `None` and the caller
/// falls back to the rectangular form.
fn cylinder_region_integral(
    topology: &Topology,
    face: &Face,
    weight: &TrigPoly,
    power: u32,
) -> Option<f64> {
    let mut total = 0.0;
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            let range = coedge.parameter_range;
            total += match coedge.pcurve {
                Curve2::Line { .. } => {
                    let [start, end] = coedge.pcurve_endpoints();
                    let across = end.x - start.x;
                    let along = end.y - start.y;
                    let scale = across.abs().max(along.abs()).max(1.0);
                    if across.abs() <= scale * 1.0e-12 {
                        0.0
                    } else if along.abs() <= scale * 1.0e-12 {
                        let level = start.y.powi(power as i32 + 1) / f64::from(power + 1);
                        -level * weight.integrate(start.x, end.x)
                    } else {
                        return None;
                    }
                }
                Curve2::Harmonic {
                    mean,
                    amplitude,
                    phase,
                } => {
                    let (sin_phase, cos_phase) = phase.sin_cos();
                    let trace = TrigPoly::constant(mean)
                        .plus(&TrigPoly::cosine().scaled(amplitude * cos_phase))
                        .plus(&TrigPoly::sine().scaled(amplitude * sin_phase));
                    let integrand = weight
                        .times(&trace.power(power + 1))
                        .scaled(-1.0 / f64::from(power + 1));
                    integrand.integrate(range.start, range.end)
                }
                Curve2::Circle { .. } => return None,
                Curve2::Ellipse { .. } => return None,
            };
        }
    }
    total.is_finite().then_some(total)
}

struct CylinderContourMeasures {
    area: f64,
    flux: f64,
    moment: Vector3,
}

/// Area, flux, and first moment of a cylinder face over an arbitrary
/// parameter region, with `x = origin − anchor + R·r̂(θ) + v·axis` and
/// `n dA = sign·r̂(θ)·R dθ dv`, where `r̂(θ) = cos θ·u + sign·sin θ·v`.
fn cylinder_contour_measures(
    topology: &Topology,
    face: &Face,
    cylinder: crate::topology::Cylinder,
    anchor: Point3,
) -> Option<CylinderContourMeasures> {
    let sign = cylinder.angular_sign;
    let radius = cylinder.radius;
    let offset = cylinder.origin - anchor;
    let along_u = offset.dot(cylinder.radial_u);
    let along_v = offset.dot(cylinder.radial_v);
    let axial = offset.dot(cylinder.axis);
    let one = TrigPoly::constant(1.0);
    let cosine = TrigPoly::cosine();
    let sine = TrigPoly::sine().scaled(sign);
    let integral =
        |weight: &TrigPoly, power: u32| cylinder_region_integral(topology, face, weight, power);
    // The radial unit vector integrated against v^k.
    let radial = |power: u32| -> Option<Vector3> {
        Some(
            cylinder.radial_u * integral(&cosine, power)?
                + cylinder.radial_v * integral(&sine, power)?,
        )
    };
    let area = radius * integral(&one, 0)?;
    // (x − p)·n = offset·r̂ + R.
    let offset_radial = TrigPoly::constant(along_u)
        .times(&cosine)
        .plus(&TrigPoly::constant(along_v).times(&sine));
    let flux = sign * radius * (integral(&offset_radial, 0)? + radius * integral(&one, 0)?);
    // |x|² = |o|² + R² + v² + 2R(o·r̂) + 2v(o·a); moment = ½∮|x|² n dA.
    let constant = offset.dot(offset) + radius * radius;
    let radial_0 = radial(0)?;
    let radial_1 = radial(1)?;
    let radial_2 = radial(2)?;
    let cross_u = cylinder.radial_u * integral(&offset_radial.times(&cosine), 0)?;
    let cross_v = cylinder.radial_v * integral(&offset_radial.times(&sine), 0)?;
    let moment = (radial_0 * constant
        + radial_2
        + (cross_u + cross_v) * (2.0 * radius)
        + radial_1 * (2.0 * axial))
        * (sign * radius / 2.0);
    Some(CylinderContourMeasures { area, flux, moment })
}

fn azimuth_moments(
    radial_u: Vector3,
    radial_v: Vector3,
    sign: f64,
    offset: Vector3,
    u_min: f64,
    u_max: f64,
) -> AzimuthMoments {
    let (sin_min, cos_min) = (sign * u_min).sin_cos();
    let (sin_max, cos_max) = (sign * u_max).sin_cos();
    let cosine = (sin_max - sin_min) / sign;
    let sine = (cos_min - cos_max) / sign;
    let half = (u_max - u_min) / 2.0;
    // ∫cos² and ∫sin² share a half-range term and differ by the double-angle
    // wobble; ∫sin·cos is the same double-angle integral in another guise.
    let wobble = (sin_max * cos_max - sin_min * cos_min) / (2.0 * sign);
    let cosine_square = half + wobble;
    let sine_square = half - wobble;
    let mixed = sin_max.mul_add(sin_max, -(sin_min * sin_min)) / (2.0 * sign);
    let along_u = offset.dot(radial_u);
    let along_v = offset.dot(radial_v);
    AzimuthMoments {
        sweep: u_max - u_min,
        radial: radial_u * cosine + radial_v * sine,
        radial_offset: radial_u * along_u.mul_add(cosine_square, along_v * mixed)
            + radial_v * along_u.mul_add(mixed, along_v * sine_square),
        offset_radial: along_u.mul_add(cosine, along_v * sine),
    }
}

/// Assembles one face's first moment from its azimuth integrals and the four
/// latitude integrals `[∫ρz'g, ∫ρ²z', ∫ρρ'g, ∫ρ²ρ']`, where
/// `g = |offset|² + ρ² + z² + 2z(offset·axis)`.
fn revolution_moment(
    sign: f64,
    azimuth: &AzimuthMoments,
    axis: Vector3,
    latitude: [f64; 4],
) -> Vector3 {
    let [along, across, axial_along, axial_across] = latitude;
    (azimuth.radial * along + azimuth.radial_offset * (2.0 * across)
        - axis
            * azimuth
                .sweep
                .mul_add(axial_along, 2.0 * azimuth.offset_radial * axial_across))
        * (sign / 2.0)
}

/// `\int_{from}^{to} cos^a(t) sin^b(t) dt` for `a + b <= 3`, in closed form.
///
/// Every moment integral in this module — the parameter loop's polar second
/// moment and the sphere and torus latitude moments alike — reduces to one of
/// these ten cases, so they are written once and shared rather than expanded
/// by hand at each call site.
fn trig_moment(cosines: u32, sines: u32, from: f64, to: f64) -> f64 {
    let antiderivative = |t: f64| {
        let (sin, cos) = t.sin_cos();
        match (cosines, sines) {
            (0, 0) => t,
            (1, 0) => sin,
            (0, 1) => -cos,
            (2, 0) => (t + sin * cos) / 2.0,
            (1, 1) => sin * sin / 2.0,
            (0, 2) => (t - sin * cos) / 2.0,
            (3, 0) => sin - sin.powi(3) / 3.0,
            (2, 1) => -cos.powi(3) / 3.0,
            (1, 2) => sin.powi(3) / 3.0,
            (0, 3) => cos.powi(3) / 3.0 - cos,
            _ => f64::NAN,
        }
    };
    antiderivative(to) - antiderivative(from)
}

/// The signed polar second moment `\int\int |w|^2 dA` of a face's parameter
/// region, in raw parameter coordinates.
///
/// Evaluated as the contour integral `1/4 \oint |w|^2 (w x dw)`, which needs
/// no decomposition of the region: each straight pcurve contributes a cubic
/// in its own parameter and each circular pcurve a trigonometric polynomial of
/// degree three, both closed form. The result carries the loop's orientation,
/// so a clockwise face reports a negative moment exactly as its area does.
///
/// The contour is walked about the loop's own start point so a region far from
/// the parameter origin keeps its precision, then shifted back to raw
/// coordinates — the convention [`face_parameter_area_and_moment`] already
/// returns, so the two compose without the caller tracking an anchor.
fn face_parameter_polar_moment(topology: &Topology, face: &Face) -> Option<f64> {
    let outer = topology.loop_record(face.outer_loop)?;
    let first_coedge = topology.coedge(*outer.value.coedges.first()?)?.value;
    let origin = first_coedge
        .pcurve
        .evaluate(first_coedge.parameter_range.start);
    let mut total = 0.0;
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            let range = coedge.parameter_range;
            total += match coedge.pcurve {
                // Harmonics live on cylinders and cones; the polar moment is a
                // planar quantity, so a face carrying one is outside this form.
                Curve2::Harmonic { .. } => return None,
                Curve2::Line { .. } => {
                    let from = coedge.pcurve.evaluate(range.start);
                    let to = coedge.pcurve.evaluate(range.end);
                    let start = Point2::new(from.x - origin.x, from.y - origin.y);
                    let delta = Point2::new(to.x - from.x, to.y - from.y);
                    // |w|^2 is quadratic in t and (w x dw) is constant, so the
                    // whole contribution closes in one cubic.
                    let cross = start.x.mul_add(delta.y, -(start.y * delta.x));
                    let projection = start.x.mul_add(delta.x, start.y * delta.y);
                    let square = delta.x.mul_add(delta.x, delta.y * delta.y);
                    cross
                        * start
                            .x
                            .mul_add(start.x, start.y.mul_add(start.y, projection + square / 3.0))
                        / 4.0
                }
                Curve2::Circle {
                    center,
                    u,
                    v,
                    radius,
                } => {
                    let centre = Point2::new(center.x - origin.x, center.y - origin.y);
                    let determinant = u.x.mul_add(v.y, -(u.y * v.x));
                    // |w|^2 coefficients against {1, cos, sin, cos^2, sin cos,
                    // sin^2}, and (w x dw) against {1, cos, sin}.
                    let square = [
                        centre.x.mul_add(centre.x, centre.y * centre.y),
                        2.0 * radius * centre.x.mul_add(u.x, centre.y * u.y),
                        2.0 * radius * centre.x.mul_add(v.x, centre.y * v.y),
                        radius * radius * u.x.mul_add(u.x, u.y * u.y),
                        2.0 * radius * radius * u.x.mul_add(v.x, u.y * v.y),
                        radius * radius * v.x.mul_add(v.x, v.y * v.y),
                    ];
                    let turn = [
                        radius * radius * determinant,
                        radius * centre.x.mul_add(v.y, -(centre.y * v.x)),
                        -radius * centre.x.mul_add(u.y, -(centre.y * u.x)),
                    ];
                    // Monomial powers of the six-by-three product, collected.
                    let powers: [(u32, u32); 6] = [(0, 0), (1, 0), (0, 1), (2, 0), (1, 1), (0, 2)];
                    let turn_powers: [(u32, u32); 3] = [(0, 0), (1, 0), (0, 1)];
                    let mut sum = 0.0;
                    for (left, (left_cos, left_sin)) in square.iter().zip(powers) {
                        for (right, (right_cos, right_sin)) in turn.iter().zip(turn_powers) {
                            sum += left
                                * right
                                * trig_moment(
                                    left_cos + right_cos,
                                    left_sin + right_sin,
                                    range.start,
                                    range.end,
                                );
                        }
                    }
                    sum / 4.0
                }
                Curve2::Ellipse {
                    center,
                    u,
                    v,
                    major_radius,
                    minor_radius,
                } => {
                    // `¼∮|w|²(w × dw)` with `w = c + a cos t u + b sin t v`,
                    // a trigonometric polynomial of the parameter.
                    let centre = Point2::new(center.x - origin.x, center.y - origin.y);
                    let x = TrigPoly::constant(centre.x)
                        .plus(&TrigPoly::cosine().scaled(major_radius * u.x))
                        .plus(&TrigPoly::sine().scaled(minor_radius * v.x));
                    let y = TrigPoly::constant(centre.y)
                        .plus(&TrigPoly::cosine().scaled(major_radius * u.y))
                        .plus(&TrigPoly::sine().scaled(minor_radius * v.y));
                    let dx = TrigPoly::sine()
                        .scaled(-major_radius * u.x)
                        .plus(&TrigPoly::cosine().scaled(minor_radius * v.x));
                    let dy = TrigPoly::sine()
                        .scaled(-major_radius * u.y)
                        .plus(&TrigPoly::cosine().scaled(minor_radius * v.y));
                    let square = x.power(2).plus(&y.power(2));
                    let turn = x.times(&dy).plus(&y.times(&dx).scaled(-1.0));
                    square
                        .times(&turn)
                        .scaled(0.25)
                        .integrate(range.start, range.end)
                }
            };
        }
    }
    // Shift from the loop-local anchor `a` back to raw coordinates:
    // ∫∫|w' + a|² = ∫∫|w'|² + 2a·∫∫w' + |a|²A, and ∫∫w' is the raw first
    // moment less `a·A`.
    let (area, first) = face_parameter_area_and_moment(topology, face)?;
    let total = origin
        .x
        .mul_add(2.0 * first.x, origin.y.mul_add(2.0 * first.y, total))
        - origin.x.mul_add(origin.x, origin.y * origin.y) * area;
    total.is_finite().then_some(total)
}

/// Parameter-space extent of a face's outer loop.
pub(crate) fn pcurve_extent(topology: &Topology, face: &Face) -> Option<(f64, f64, f64, f64)> {
    let loop_record = topology.loop_record(face.outer_loop)?;
    let mut u_min = f64::INFINITY;
    let mut u_max = f64::NEG_INFINITY;
    let mut v_min = f64::INFINITY;
    let mut v_max = f64::NEG_INFINITY;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology.coedge(*coedge_key)?.value;
        for point in coedge.pcurve_endpoints() {
            u_min = u_min.min(point.x);
            u_max = u_max.max(point.x);
            v_min = v_min.min(point.y);
            v_max = v_max.max(point.y);
        }
    }
    (u_min < u_max && v_min < v_max).then_some((u_min, u_max, v_min, v_max))
}

fn calculate_analytic_face_feature_measures(
    topology: &Topology,
    bounds: Option<Bounds3>,
) -> Option<ShapeMeasures> {
    if topology
        .solids
        .iter()
        .any(|solid| !solid.value.inner_shells.is_empty())
    {
        return None;
    }
    let shell_faces = topology
        .shells
        .iter()
        .flat_map(|shell| shell.value.faces.iter().copied())
        .collect::<BTreeSet<_>>();
    let latest_side = shell_faces.iter().rev().copied().find(|key| {
        topology.face(*key).is_some_and(|face| {
            matches!(face.value.role, crate::topology::FaceRole::FeatureSide(_))
        })
    })?;
    let latest_index = latest_side.0;
    let mut expected_ordinal = match topology.face(latest_side)?.value.role {
        crate::topology::FaceRole::FeatureSide(ordinal) => ordinal,
        _ => return None,
    };
    let mut first_index = latest_index;
    loop {
        if first_index == 0 || !shell_faces.contains(&FaceKey(first_index - 1)) {
            break;
        }
        let previous = topology.face(FaceKey(first_index - 1))?;
        let crate::topology::FaceRole::FeatureSide(previous_ordinal) = previous.value.role else {
            break;
        };
        if previous_ordinal == expected_ordinal {
            // Several exact patches may share one logical carrier (a full
            // circle is stored as two semicircular faces). Keep the complete
            // contiguous carrier in the analytic measure block.
            first_index -= 1;
        } else if expected_ordinal > 0 && previous_ordinal == expected_ordinal - 1 {
            expected_ordinal = previous_ordinal;
            first_index -= 1;
        } else {
            break;
        }
    }
    let feature_side_keys = (first_index..=latest_index)
        .map(FaceKey)
        .filter(|key| shell_faces.contains(key))
        .collect::<BTreeSet<_>>();
    if feature_side_keys.is_empty() {
        return None;
    }

    let first_side = topology.face(*feature_side_keys.first()?)?;
    let direction = match first_side.value.surface {
        Surface::Plane(plane) => plane.v,
        Surface::Cylinder(cylinder) => cylinder.axis,
        Surface::Torus(torus) => torus.axis,
        Surface::Cone(cone) => cone.axis,
        Surface::Sphere(sphere) => sphere.axis,
    };
    let direction = robust_normalized(direction)?;
    let mut start_edges = BTreeSet::new();
    let mut end_edges = BTreeSet::new();
    let mut height = None::<f64>;
    for face_key in &feature_side_keys {
        let face = topology.face(*face_key)?;
        let profile_loop = topology.loop_record(face.value.outer_loop)?;
        if profile_loop.value.coedges.len() != 4 {
            return None;
        }
        start_edges.insert(topology.coedge(profile_loop.value.coedges[0])?.value.edge);
        end_edges.insert(topology.coedge(profile_loop.value.coedges[2])?.value.edge);
        let extent = profile_loop
            .value
            .coedges
            .iter()
            .filter_map(|key| topology.coedge(*key))
            .flat_map(|coedge| coedge.value.pcurve_endpoints())
            .map(|point| point.y)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        let extent = extent.1 - extent.0;
        if !extent.is_finite() || extent <= 0.0 {
            return None;
        }
        if height.is_some_and(|expected| (expected - extent).abs() > 256.0 * f64::EPSILON) {
            return None;
        }
        height = Some(extent);
    }
    let height = height?;
    let boundary_edges = start_edges
        .iter()
        .chain(&end_edges)
        .copied()
        .collect::<BTreeSet<_>>();
    let feature_loop_keys = topology
        .loops
        .iter()
        .enumerate()
        .filter_map(|(index, profile_loop)| {
            (!profile_loop.value.coedges.is_empty()
                && profile_loop.value.coedges.iter().all(|coedge_key| {
                    boundary_edges.contains(&topology.coedges[coedge_key.0].value.edge)
                }))
            .then_some(LoopKey(index))
        })
        .collect::<BTreeSet<_>>();
    let feature_end_keys = shell_faces
        .iter()
        .copied()
        .filter(|key| {
            topology.face(*key).is_some_and(|face| {
                face.value.role == crate::topology::FaceRole::FeatureEnd
                    && feature_loop_keys.contains(&face.value.outer_loop)
            })
        })
        .collect::<BTreeSet<_>>();
    let complement_face_keys = shell_faces
        .iter()
        .copied()
        .filter(|key| {
            topology.face(*key).is_some_and(|face| {
                face.value.role != crate::topology::FaceRole::FeatureEnd
                    && matches!(face.value.surface, Surface::Plane(_))
                    && feature_loop_keys.contains(&face.value.outer_loop)
            })
        })
        .collect::<BTreeSet<_>>();

    let (profile_area, profile_moment, profile_perimeter, start_centroid) =
        exact_feature_profile_measures(topology, &start_edges, direction)?;
    if !profile_area.is_finite() || profile_area == 0.0 || profile_perimeter <= 0.0 {
        return None;
    }

    let mut base = topology.clone();
    let removed_faces = feature_side_keys
        .iter()
        .chain(&feature_end_keys)
        .chain(&complement_face_keys)
        .copied()
        .collect::<BTreeSet<_>>();
    let root_faces = shell_faces
        .iter()
        .copied()
        .filter(|key| !removed_faces.contains(key))
        .filter(|key| {
            topology.face(*key).is_some_and(|face| {
                face.value
                    .inner_loops
                    .iter()
                    .any(|loop_key| feature_loop_keys.contains(loop_key))
            })
        })
        .collect::<Vec<_>>();
    let inherited = complement_face_keys
        .iter()
        .flat_map(|key| {
            let face = &topology.faces[key.0].value;
            face.inner_loops
                .iter()
                .filter(|loop_key| !feature_loop_keys.contains(loop_key))
                .copied()
                .map(move |loop_key| (face.surface, loop_key))
        })
        .collect::<Vec<_>>();
    for shell in &mut base.shells {
        shell.value.faces.retain(|key| !removed_faces.contains(key));
    }
    for face_key in &root_faces {
        base.faces[face_key.0]
            .value
            .inner_loops
            .retain(|loop_key| !feature_loop_keys.contains(loop_key));
    }
    for (surface, loop_key) in inherited {
        let root = root_faces
            .iter()
            .copied()
            .find(|key| topology.faces[key.0].value.surface == surface)?;
        base.faces[root.0].value.inner_loops.push(loop_key);
    }

    let base_measures = calculate_measures(&base);
    let feature_volume = profile_area * height;
    let signed_volume = base_measures.signed_volume + feature_volume;
    if !signed_volume.is_finite() || signed_volume <= 0.0 {
        return None;
    }
    let feature_centroid = start_centroid + direction * (0.5 * height);
    let base_centroid = base_measures.centroid?;
    let centroid = (base_centroid.as_vector() * base_measures.signed_volume
        + feature_centroid.as_vector() * feature_volume)
        / signed_volume;
    let through = feature_end_keys.is_empty();
    let surface_delta = profile_perimeter * height
        - if through {
            2.0 * profile_area.abs()
        } else {
            0.0
        };
    let _ = profile_moment;
    Some(ShapeMeasures {
        bounds,
        surface_area: base_measures.surface_area + surface_delta,
        signed_volume,
        centroid: Some(Point3::new(centroid.x, centroid.y, centroid.z)),
    })
}

fn exact_feature_profile_measures(
    topology: &Topology,
    start_edges: &BTreeSet<EdgeKey>,
    direction: Vector3,
) -> Option<(f64, Vector2, f64, Point3)> {
    let first_edge = topology.edge(*start_edges.first()?)?.value;
    let origin = first_edge.endpoints()[0];
    let reference =
        if direction.x.abs() <= direction.y.abs() && direction.x.abs() <= direction.z.abs() {
            Vector3::new(1.0, 0.0, 0.0)
        } else if direction.y.abs() <= direction.z.abs() {
            Vector3::new(0.0, 1.0, 0.0)
        } else {
            Vector3::new(0.0, 0.0, 1.0)
        };
    let u = robust_normalized(reference - direction * reference.dot(direction))?;
    let v = direction.cross(u);
    let project = |point: Point3| {
        let relative = point - origin;
        Point2::new(relative.dot(u), relative.dot(v))
    };
    let mut area = 0.0;
    let mut moment = Vector2::new(0.0, 0.0);
    let mut perimeter = 0.0;
    for edge_key in start_edges {
        let edge = topology.edge(*edge_key)?.value;
        let start = project(edge.curve.evaluate(edge.parameter_range.start));
        let end = project(edge.curve.evaluate(edge.parameter_range.end));
        let chord_cross = start.x * end.y - start.y * end.x;
        let chord_area = 0.5 * chord_cross;
        area += chord_area;
        moment.x += chord_cross * (start.x + end.x) / 6.0;
        moment.y += chord_cross * (start.y + end.y) / 6.0;
        perimeter += edge.length();
        if let Curve3::Circle {
            center,
            u: circle_u,
            v: circle_v,
            radius,
        } = edge.curve
        {
            let center = project(center);
            let projected_u = Vector2::new(circle_u.dot(u), circle_u.dot(v));
            let projected_v = Vector2::new(circle_v.dot(u), circle_v.dot(v));
            let sweep = edge.parameter_range.end - edge.parameter_range.start;
            let determinant = projected_u.x * projected_v.y - projected_u.y * projected_v.x;
            let segment_area = 0.5 * radius * radius * determinant * (sweep - sweep.sin());
            let middle = 0.5 * (edge.parameter_range.start + edge.parameter_range.end);
            let radial = Vector2::new(
                projected_u.x * middle.cos() + projected_v.x * middle.sin(),
                projected_u.y * middle.cos() + projected_v.y * middle.sin(),
            );
            let offset_scale =
                determinant * (2.0 / 3.0) * radius.powi(3) * (0.5 * sweep).sin().powi(3);
            area += segment_area;
            moment.x += center.x * segment_area + radial.x * offset_scale;
            moment.y += center.y * segment_area + radial.y * offset_scale;
        }
    }
    if !area.is_finite() || area == 0.0 || !moment.is_finite() || !perimeter.is_finite() {
        return None;
    }
    let planar_centroid = Point2::new(moment.x / area, moment.y / area);
    let centroid = origin + u * planar_centroid.x + v * planar_centroid.y;
    Some((area, moment, perimeter, centroid))
}

fn robust_normalized(vector: Vector3) -> Option<Vector3> {
    let scale = vector.x.abs().max(vector.y.abs()).max(vector.z.abs());
    if !scale.is_finite() || scale == 0.0 {
        return None;
    }
    let scaled = vector / scale;
    let length = scaled.length();
    (length.is_finite() && length > 0.0).then_some(scaled / length)
}

fn calculate_analytic_extrusion_measures(
    topology: &Topology,
    bounds: Option<Bounds3>,
) -> Option<ShapeMeasures> {
    if topology
        .solids
        .iter()
        .any(|solid| !solid.value.inner_shells.is_empty())
    {
        // A cavity is not an extrusion; the flux engines handle it.
        return None;
    }
    let mut surface_area = 0.0;
    let mut signed_volume = 0.0;
    let mut centroid_numerator = Vector3::default();
    for solid in &topology.solids {
        let shell = topology.shell(solid.value.outer_shell)?;
        let faces = shell
            .value
            .faces
            .iter()
            .filter_map(|face_key| topology.face(*face_key))
            .collect::<Vec<_>>();
        let top = faces
            .iter()
            .find(|face| face.value.role == crate::topology::FaceRole::ExtrusionTop)?;
        let bottom = faces
            .iter()
            .find(|face| face.value.role == crate::topology::FaceRole::ExtrusionBottom)?;
        let top_plane = top.value.surface.as_plane()?;
        let bottom_plane = bottom.value.surface.as_plane()?;
        let height = (top_plane.origin - bottom_plane.origin)
            .dot(top_plane.normal)
            .abs();
        if !height.is_finite() || height == 0.0 {
            return None;
        }
        let (profile_area, profile_moment) = face_parameter_area_and_moment(topology, &top.value)?;
        if !profile_area.is_finite() || profile_area <= 0.0 {
            return None;
        }
        let planar_centroid = Point2::new(
            profile_moment.x / profile_area,
            profile_moment.y / profile_area,
        );
        let top_centroid = top_plane.evaluate(planar_centroid);
        let centroid = top_centroid + top_plane.normal * (-0.5 * height);
        let volume = profile_area * height;
        signed_volume += volume;
        centroid_numerator = centroid_numerator + centroid.as_vector() * volume;

        for face in faces {
            let parameter_area = face_parameter_area_and_moment(topology, &face.value)?
                .0
                .abs();
            let jacobian = match face.value.surface {
                Surface::Plane(plane) => plane.u.cross(plane.v).length(),
                Surface::Cylinder(cylinder) => cylinder.radius * cylinder.axis.length(),
                // Blend surfaces belong to the exact shell measure strategy.
                Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => return None,
            };
            surface_area += parameter_area * jacobian;
        }
    }
    let centroid = centroid_numerator / signed_volume;
    (surface_area.is_finite() && signed_volume.is_finite()).then_some(ShapeMeasures {
        bounds,
        surface_area,
        signed_volume,
        centroid: Some(Point3::new(centroid.x, centroid.y, centroid.z)),
    })
}

pub(crate) fn face_parameter_area_and_moment(
    topology: &Topology,
    face: &Face,
) -> Option<(f64, Vector2)> {
    let outer = topology.loop_record(face.outer_loop)?;
    let first_coedge = topology.coedge(*outer.value.coedges.first()?)?.value;
    let anchor = first_coedge
        .pcurve
        .evaluate(first_coedge.parameter_range.start);
    let mut area = 0.0;
    let mut moment = Vector2::new(0.0, 0.0);
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            let world_start = coedge.pcurve.evaluate(coedge.parameter_range.start);
            let world_end = coedge.pcurve.evaluate(coedge.parameter_range.end);
            let start = Point2::new(world_start.x - anchor.x, world_start.y - anchor.y);
            let end = Point2::new(world_end.x - anchor.x, world_end.y - anchor.y);
            let chord_cross = start.x * end.y - start.y * end.x;
            let chord_area = 0.5 * chord_cross;
            area += chord_area;
            moment.x += chord_cross * (start.x + end.x) / 6.0;
            moment.y += chord_cross * (start.y + end.y) / 6.0;

            if let Curve2::Circle {
                center,
                u,
                v,
                radius,
            } = coedge.pcurve
            {
                let center = Point2::new(center.x - anchor.x, center.y - anchor.y);
                let sweep = coedge.parameter_range.end - coedge.parameter_range.start;
                let determinant = u.x * v.y - u.y * v.x;
                let segment_area = 0.5 * radius * radius * determinant * (sweep - sweep.sin());
                let middle = 0.5 * (coedge.parameter_range.start + coedge.parameter_range.end);
                let direction = Vector2::new(
                    u.x * middle.cos() + v.x * middle.sin(),
                    u.y * middle.cos() + v.y * middle.sin(),
                );
                let offset_scale =
                    determinant * (2.0 / 3.0) * radius.powi(3) * (0.5 * sweep).sin().powi(3);
                area += segment_area;
                moment.x += center.x * segment_area + direction.x * offset_scale;
                moment.y += center.y * segment_area + direction.y * offset_scale;
            }
            if let Curve2::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            } = coedge.pcurve
            {
                // The exact contour terms of the elliptical arc, `∮x dy`,
                // `∮½x² dy` and `−∮½y² dx`, integrate in closed form as
                // trigonometric polynomials of the parameter; the chord's
                // share, counted above, comes off again.
                let center = Point2::new(center.x - anchor.x, center.y - anchor.y);
                let x = TrigPoly::constant(center.x)
                    .plus(&TrigPoly::cosine().scaled(major_radius * u.x))
                    .plus(&TrigPoly::sine().scaled(minor_radius * v.x));
                let y = TrigPoly::constant(center.y)
                    .plus(&TrigPoly::cosine().scaled(major_radius * u.y))
                    .plus(&TrigPoly::sine().scaled(minor_radius * v.y));
                let dx = TrigPoly::sine()
                    .scaled(-major_radius * u.x)
                    .plus(&TrigPoly::cosine().scaled(minor_radius * v.x));
                let dy = TrigPoly::sine()
                    .scaled(-major_radius * u.y)
                    .plus(&TrigPoly::cosine().scaled(minor_radius * v.y));
                let (from, to) = (coedge.parameter_range.start, coedge.parameter_range.end);
                let exact_area = x.times(&dy).integrate(from, to);
                let exact_moment_x = x.power(2).times(&dy).scaled(0.5).integrate(from, to);
                let exact_moment_y = y.power(2).times(&dx).scaled(-0.5).integrate(from, to);
                area += exact_area - chord_area;
                moment.x += exact_moment_x - chord_cross * (start.x + end.x) / 6.0;
                moment.y += exact_moment_y - chord_cross * (start.y + end.y) / 6.0;
            }
            if let Curve2::Harmonic {
                mean,
                amplitude,
                phase,
            } = coedge.pcurve
            {
                // The exact contour term of the harmonic, less the chord
                // already counted, in the anchored frame (the anchor shift
                // changes only the moment, which harmonics never feed).
                let exact = harmonic_area_contribution(
                    mean - anchor.y,
                    amplitude,
                    phase,
                    coedge.parameter_range.start - anchor.x,
                    coedge.parameter_range.end - anchor.x,
                );
                area += exact - chord_area;
            }
        }
    }
    moment.x += anchor.x * area;
    moment.y += anchor.y * area;
    (area.is_finite() && moment.is_finite()).then_some((area, moment))
}

fn calculate_bounds(topology: &Topology) -> Option<Bounds3> {
    let mut points = topology.vertices.iter().map(|vertex| vertex.value.point);
    let first = points.next()?;
    if !first.is_finite() {
        return None;
    }
    let mut min = first;
    let mut max = first;
    for point in points {
        if !point.is_finite() {
            return None;
        }
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        min.z = min.z.min(point.z);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
        max.z = max.z.max(point.z);
    }
    for edge in &topology.edges {
        // Each coordinate of `c + a cos t u + b sin t v` is extremal where
        // `tan t = (b v_i) / (a u_i)`; a circle is the case `a = b`.
        let (u, v, major, minor) = match edge.value.curve {
            Curve3::Circle { u, v, radius, .. } => (u, v, radius, radius),
            Curve3::Ellipse {
                u,
                v,
                major_radius,
                minor_radius,
                ..
            } => (u, v, major_radius, minor_radius),
            Curve3::Line { .. } => continue,
        };
        for (u_component, v_component) in [(u.x, v.x), (u.y, v.y), (u.z, v.z)] {
            if u_component == 0.0 && v_component == 0.0 {
                continue;
            }
            let extremum = (minor * v_component).atan2(major * u_component);
            for angle in [extremum, extremum + std::f64::consts::PI] {
                if angle_is_on_range(angle, edge.value.parameter_range) {
                    let point = edge.value.curve.evaluate(angle);
                    min.x = min.x.min(point.x);
                    min.y = min.y.min(point.y);
                    min.z = min.z.min(point.z);
                    max.x = max.x.max(point.x);
                    max.y = max.y.max(point.y);
                    max.z = max.z.max(point.z);
                }
            }
        }
    }
    Some(Bounds3 { min, max })
}

fn angle_is_on_range(angle: f64, range: crate::topology::ParameterRange) -> bool {
    let sweep = range.end - range.start;
    let directed = if sweep >= 0.0 {
        (angle - range.start).rem_euclid(std::f64::consts::TAU)
    } else {
        (range.start - angle).rem_euclid(std::f64::consts::TAU)
    };
    directed <= sweep.abs() + 64.0 * f64::EPSILON
}

pub(crate) fn face_polygon(topology: &Topology, loop_key: LoopKey) -> Option<Vec<Point3>> {
    let loop_record = topology.loop_record(loop_key)?;
    loop_record
        .value
        .coedges
        .iter()
        .map(|coedge_key| {
            let coedge = topology.coedge(*coedge_key)?;
            let (_, endpoints) = topology.oriented_edge_vertices(&coedge.value)?;
            Some(endpoints[0])
        })
        .collect()
}

pub(crate) fn face_boundary_polygons(topology: &Topology, face: &Face) -> Option<Vec<Vec<Point3>>> {
    face.loops()
        .map(|loop_key| face_polygon(topology, loop_key))
        .collect()
}

fn polygon_area_vector(points: &[Point3]) -> Vector3 {
    if points.len() < 3 {
        return Vector3::default();
    }
    // Anchor locally so a small face translated far from the origin retains
    // its exact area instead of subtracting large world-coordinate products.
    let anchor = points[0];
    let mut doubled_area = Vector3::default();
    for index in 1..points.len() - 1 {
        doubled_area = doubled_area + (points[index] - anchor).cross(points[index + 1] - anchor);
    }
    doubled_area * 0.5
}

#[cfg(test)]
pub(crate) mod malformed {
    use crate::topology::{
        CoedgeKey, Curve2, Curve3, EdgeKey, Orientation, ParameterRange, Point2, Topology, Vector2,
        VertexKey,
    };

    pub(crate) fn dangling_vertex(topology: &mut Topology) {
        topology.edges[0].value.vertices[0] = VertexKey(usize::MAX);
    }

    pub(crate) fn endpoint_mismatch(topology: &mut Topology) {
        let Curve3::Line { endpoints } = &mut topology.edges[0].value.curve else {
            panic!("cuboid fixture edge is linear");
        };
        endpoints[0].x += 0.25;
    }

    pub(crate) fn open_loop(topology: &mut Topology) {
        topology.loops[0].value.coedges.swap(1, 2);
    }

    pub(crate) fn edge_used_once(topology: &mut Topology) {
        let edge = topology.coedges[0].value.edge;
        let replacement = topology
            .coedges
            .iter()
            .find(|coedge| coedge.value.edge != edge)
            .map(|coedge| coedge.value.edge)
            .expect("cuboid has another edge");
        topology.coedges[0].value.edge = replacement;
    }

    pub(crate) fn same_edge_use_orientation(topology: &mut Topology) {
        let edge = EdgeKey(0);
        let mut uses = topology
            .coedges
            .iter_mut()
            .filter(|coedge| coedge.value.edge == edge)
            .collect::<Vec<_>>();
        uses[0].value.orientation = Orientation::Forward;
        uses[1].value.orientation = Orientation::Forward;
    }

    pub(crate) fn pcurve_mismatch(topology: &mut Topology) {
        let Curve2::Line { endpoints } = &mut topology.coedges[0].value.pcurve else {
            panic!("cuboid fixture pcurve is linear");
        };
        endpoints[0] = Point2::new(123.0, 456.0);
    }

    pub(crate) fn bowed_pcurve_with_matching_endpoints(topology: &mut Topology) {
        let endpoints = topology.coedges[0].value.pcurve_endpoints();
        let center = Point2::new(
            0.5 * (endpoints[0].x + endpoints[1].x),
            0.5 * (endpoints[0].y + endpoints[1].y),
        );
        let radial = endpoints[0] - center;
        let radius = radial.x.hypot(radial.y);
        let u = Vector2::new(radial.x / radius, radial.y / radius);
        let v = Vector2::new(-u.y, u.x);
        topology.coedges[0].value.pcurve = Curve2::Circle {
            center,
            u,
            v,
            radius,
        };
        topology.coedges[0].value.parameter_range = ParameterRange::new(0.0, std::f64::consts::PI);
    }

    pub(crate) fn reverse_face(topology: &mut Topology) {
        let loop_key = topology.faces[0].value.outer_loop;
        let coedges = &mut topology.loops[loop_key.0].value.coedges;
        coedges.reverse();
        for coedge_key in coedges.iter().copied() {
            let coedge = &mut topology.coedges[coedge_key.0].value;
            coedge.orientation = coedge.orientation.reversed();
            coedge.parameter_range = coedge.parameter_range.reversed();
        }
    }

    pub(crate) fn dangling_coedge(topology: &mut Topology) {
        topology.loops[0].value.coedges[0] = CoedgeKey(usize::MAX);
    }
}

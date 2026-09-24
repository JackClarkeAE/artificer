//! Thicken (ADR 0056, S4): a sheet made into a solid by offsetting it.
//!
//! The solid is the material between the sheet and its offset by the
//! thickness along the sheet's normal. Its faces are the sheet's own faces
//! turned over to face into the material, the offsets of those faces, and
//! a wall along every boundary edge between the edge and its offset.
//!
//! An offset of a plane, a cylinder, a cone, a sphere or a torus is a
//! carrier of the same class in the same parameterisation: a plane moved
//! along its normal, a cylinder's radius grown, a cone's origin slid along
//! its axis and its base radius grown, a sphere's or a torus's tube radius
//! grown. So every offset face keeps its loops' curves-on-surface to the
//! bit, and its edges are the sheet's edges carried along the normal:
//! lines to lines, and circles to the circles at the same parameter.
//!
//! A wall follows the normal along a boundary edge. Along a straight edge
//! the normal is one direction and the wall is a plane. Along a circle the
//! normal keeps one radial and one axial component all the way round: with
//! no axial component the wall is a planar annulus, with no radial one a
//! cylinder, and otherwise the cone the normals sweep. Every wall is
//! therefore exact, with the pcurves written in the wall's own frame.
//!
//! Where two faces meet across an interior edge, their normals must agree
//! along it, or the two offsets would part and the wall between them is a
//! surface this release does not build; such a crease is refused by name.

use artificer_protocol::{ExecuteRequest, KernelError, KernelErrorCode, SnapshotId};

use crate::analytic_extrusion::{BoundaryUse, allocate_id, push_edge, push_loop, push_vertex};
use crate::sheet::{self, SheetResult};
use crate::topology::{
    CoedgeKey, Cone, Curve2, Curve3, Cylinder, Edge, EdgeKey, Face, FaceKey, FaceRole,
    Orientation, ParameterRange, Plane, Point2, Point3, Record, Shell, ShellKey, Solid, Surface,
    Topology, Vector2, Vector3, VertexKey, frame_orientation,
};
use crate::{CancellationToken, ExecutionOutcome, Snapshot};

/// Why a sheet could not be thickened.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ThickenError {
    ThicknessInvalid,
    FaceUnsupported,
    Crease,
    OffsetDegenerate,
    EdgeUnsupported,
    VertexUnsupported,
    Reversal,
}

impl ThickenError {
    fn refuse(self, snapshot: SnapshotId) -> KernelError {
        let (code, name, message) = match self {
            Self::ThicknessInvalid => (
                KernelErrorCode::InvalidInput,
                "THICKEN_THICKNESS_INVALID",
                "A thickness is a finite length beyond the minimum feature size, positive along the sheet's normal or negative against it.",
            ),
            Self::FaceUnsupported => (
                KernelErrorCode::Unsupported,
                "THICKEN_FACE_UNSUPPORTED",
                "Thicken offsets planes, cylinders, cones, spheres and tori exactly; a ruled or B-spline face has no exact offset in this release.",
            ),
            Self::Crease => (
                KernelErrorCode::Unsupported,
                "THICKEN_CREASE_UNSUPPORTED",
                "Two faces of the sheet meet at a crease, where their offsets part; this release thickens sheets whose faces meet smoothly.",
            ),
            Self::OffsetDegenerate => (
                KernelErrorCode::InvalidInput,
                "THICKEN_OFFSET_DEGENERATE",
                "The offset collapses a curved face: the thickness reaches or passes the face's radius.",
            ),
            Self::EdgeUnsupported => (
                KernelErrorCode::Unsupported,
                "THICKEN_EDGE_UNSUPPORTED",
                "A boundary edge is not a line or a circle whose offset is a line or a circle; the wall along it is a surface this release does not build.",
            ),
            Self::VertexUnsupported => (
                KernelErrorCode::Unsupported,
                "THICKEN_VERTEX_UNSUPPORTED",
                "The sheet has no normal at a vertex, such as a cone's apex.",
            ),
            Self::Reversal => (
                KernelErrorCode::Unsupported,
                "THICKEN_FACE_REVERSAL_UNSUPPORTED",
                "A face carries a curve-on-surface that cannot be turned over.",
            ),
        };
        sheet::refuse(snapshot, code, name, message)
    }
}

const PARALLEL: f64 = 1.0e-9;

/// `KernelCommand::ThickenSheet`.
pub(crate) fn execute_thicken(
    input: &Snapshot,
    request: &ExecuteRequest,
    cancellation: &CancellationToken,
    thickness: f64,
) -> Result<ExecutionOutcome, KernelError> {
    if !sheet::is_sheet(&input.topology) {
        return Err(sheet::not_a_sheet(input.id, "Thicken"));
    }
    let precision = request.precision;
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if !thickness.is_finite() || thickness.abs() <= minimum {
        return Err(ThickenError::ThicknessInvalid.refuse(input.id));
    }
    let mut source = input.topology.clone();
    if thickness < 0.0 {
        sheet::reverse_sheet(&mut source).map_err(|_| ThickenError::Reversal.refuse(input.id))?;
    }
    let topology =
        thicken(&source, thickness.abs(), minimum).map_err(|reason| reason.refuse(input.id))?;
    sheet::commit(
        input,
        request,
        cancellation,
        SheetResult {
            topology,
            rung: sheet::THICKEN_RUNG,
            warnings: Vec::new(),
        },
    )
}

/// The solid between `source` and its offset by `thickness` along its
/// normal.
fn thicken(source: &Topology, thickness: f64, minimum: f64) -> Result<Topology, ThickenError> {
    if source.faces.iter().any(|face| {
        matches!(
            face.value.surface,
            Surface::Ruled(_) | Surface::Bspline(_)
        )
    }) {
        return Err(ThickenError::FaceUnsupported);
    }
    let uses = sheet::edge_use_counts(source);
    // Every coedge of every edge, with its face, from the sheet as given.
    let mut edge_uses: Vec<Vec<(usize, CoedgeKey)>> = vec![Vec::new(); source.edges.len()];
    for (face_index, face) in source.faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            for coedge_key in &source.loops[loop_key.0].value.coedges {
                let edge = source.coedges[coedge_key.0].value.edge;
                edge_uses[edge.0].push((face_index, *coedge_key));
            }
        }
    }
    // Interior edges must be smooth: the offsets of a crease part.
    for (edge_index, edge) in source.edges.iter().enumerate() {
        if uses[edge_index] != 2 {
            continue;
        }
        let range = edge.value.parameter_range;
        let midpoint = edge.value.curve.evaluate((range.start + range.end) / 2.0);
        let [first, second] = [edge_uses[edge_index][0].0, edge_uses[edge_index][1].0];
        if first == second {
            continue;
        }
        let normal = |face: usize| sheet::face_normal_at(&source.faces[face].value, midpoint);
        match (normal(first), normal(second)) {
            (Some(a), Some(b)) if a.dot(b) >= 1.0 - PARALLEL => {}
            _ => return Err(ThickenError::Crease),
        }
    }

    // Offset carriers.
    let offset_surfaces: Vec<Surface> = source
        .faces
        .iter()
        .map(|face| offset_surface(face.value.surface, thickness, minimum))
        .collect::<Result<_, _>>()?;
    // A face for every vertex, to take its normal from.
    let mut vertex_face = vec![usize::MAX; source.vertices.len()];
    for (edge_index, edge) in source.edges.iter().enumerate() {
        if let Some((face, _)) = edge_uses[edge_index].first() {
            for vertex in edge.value.vertices {
                if vertex_face[vertex.0] == usize::MAX {
                    vertex_face[vertex.0] = *face;
                }
            }
        }
    }
    let offset_point = |face: usize, point: Point3| -> Result<Point3, ThickenError> {
        let normal = sheet::face_normal_at(&source.faces[face].value, point)
            .ok_or(ThickenError::VertexUnsupported)?;
        Ok(point + normal * thickness)
    };

    // The result starts as the sheet turned over: its faces become the
    // solid's inner skin.
    let mut out = source.clone();
    let mut next_id = out
        .vertices
        .iter()
        .map(|record| record.id.get())
        .chain(out.edges.iter().map(|record| record.id.get()))
        .chain(out.coedges.iter().map(|record| record.id.get()))
        .chain(out.loops.iter().map(|record| record.id.get()))
        .chain(out.faces.iter().map(|record| record.id.get()))
        .chain(out.shells.iter().map(|record| record.id.get()))
        .max()
        .unwrap_or(0)
        + 1;
    for face_index in 0..out.faces.len() {
        sheet::reverse_face(&mut out, face_index).map_err(|_| ThickenError::Reversal)?;
    }

    // Offset vertices.
    let mut offset_vertex = Vec::with_capacity(source.vertices.len());
    for (index, vertex) in source.vertices.iter().enumerate() {
        let face = vertex_face[index];
        if face == usize::MAX {
            return Err(ThickenError::VertexUnsupported);
        }
        let point = offset_point(face, vertex.value.point)?;
        offset_vertex.push(push_vertex(&mut out, &mut next_id, point));
    }
    // Offset edges: the sheet's edges carried along the normal.
    let mut offset_edge = Vec::with_capacity(source.edges.len());
    for (index, edge) in source.edges.iter().enumerate() {
        let face = edge_uses[index]
            .first()
            .map(|(face, _)| *face)
            .ok_or(ThickenError::EdgeUnsupported)?;
        let vertices = edge.value.vertices.map(|key| offset_vertex[key.0]);
        let (curve, parameter_range) = offset_curve(&edge.value, |point| {
            sheet::face_normal_at(&source.faces[face].value, point).map(|n| point + n * thickness)
        })?;
        offset_edge.push(push_edge(
            &mut out,
            &mut next_id,
            Edge {
                vertices,
                curve,
                parameter_range,
            },
        ));
    }
    // Offset faces: the same loops, the same pcurves, the offset carrier.
    let mut offset_face = Vec::with_capacity(source.faces.len());
    for (face_index, face) in source.faces.iter().enumerate() {
        let mut loops = Vec::new();
        for loop_key in face.value.loops() {
            let uses = source.loops[loop_key.0]
                .value
                .coedges
                .iter()
                .map(|coedge_key| {
                    let coedge = source.coedges[coedge_key.0].value;
                    BoundaryUse {
                        edge: offset_edge[coedge.edge.0],
                        orientation: coedge.orientation,
                        curve: (coedge.pcurve, coedge.parameter_range),
                    }
                })
                .collect();
            loops.push(push_loop(&mut out, &mut next_id, uses));
        }
        let key = FaceKey(out.faces.len());
        out.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: offset_surfaces[face_index],
                outer_loop: loops[0],
                inner_loops: loops[1..].to_vec(),
                role: face.value.role,
            },
        });
        offset_face.push(key);
    }
    // Rungs: one edge from every boundary vertex to its offset.
    let mut rung: Vec<Option<EdgeKey>> = vec![None; source.vertices.len()];
    let mut rung_at = |out: &mut Topology, next_id: &mut u64, vertex: VertexKey| -> EdgeKey {
        if let Some(edge) = rung[vertex.0] {
            return edge;
        }
        let start = out.vertices[vertex.0].value.point;
        let end = out.vertices[offset_vertex[vertex.0].0].value.point;
        let edge = push_edge(
            out,
            next_id,
            Edge::line([vertex, offset_vertex[vertex.0]], [start, end]),
        );
        rung[vertex.0] = Some(edge);
        edge
    };
    // Walls along the boundary.
    let mut wall_faces: Vec<(usize, FaceKey)> = Vec::new();
    let mut wall_ordinal = 0_u32;
    for (edge_index, edge) in source.edges.iter().enumerate() {
        if uses[edge_index] != 1 {
            continue;
        }
        let (face_index, coedge_key) = edge_uses[edge_index][0];
        let coedge = source.coedges[coedge_key.0].value;
        let face = &source.faces[face_index].value;
        let (start_vertex, end_vertex) = match coedge.orientation {
            Orientation::Forward => (edge.value.vertices[0], edge.value.vertices[1]),
            Orientation::Reverse => (edge.value.vertices[1], edge.value.vertices[0]),
        };
        let rung_start = rung_at(&mut out, &mut next_id, start_vertex);
        let rung_end = rung_at(&mut out, &mut next_id, end_vertex);
        let wall = wall_surface(face, &edge.value, coedge.orientation, thickness)?;
        let uses = vec![
            BoundaryUse {
                edge: EdgeKey(edge_index),
                orientation: coedge.orientation,
                curve: wall.along,
            },
            BoundaryUse {
                edge: rung_end,
                orientation: Orientation::Forward,
                curve: wall.rung_end,
            },
            BoundaryUse {
                edge: offset_edge[edge_index],
                orientation: coedge.orientation.reversed(),
                curve: wall.back,
            },
            BoundaryUse {
                edge: rung_start,
                orientation: Orientation::Reverse,
                curve: wall.rung_start,
            },
        ];
        let loop_key = push_loop(&mut out, &mut next_id, uses);
        let key = FaceKey(out.faces.len());
        out.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: wall.surface,
                outer_loop: loop_key,
                inner_loops: Vec::new(),
                role: FaceRole::FeatureSide(wall_ordinal),
            },
        });
        wall_ordinal += 1;
        wall_faces.push((face_index, key));
    }
    // One solid per shell of the sheet. An open shell's inner skin, outer
    // skin and walls close into one shell; a closed shell — a sphere, a
    // tube — has no walls, and its two skins are the solid's outer shell
    // and the cavity between them, whichever way round the sheet faced.
    let source_shells = source.shells.clone();
    out.shells.clear();
    out.solids.clear();
    for shell in &source_shells {
        let members: Vec<usize> = shell.value.faces.iter().map(|key| key.0).collect();
        let walls: Vec<FaceKey> = wall_faces
            .iter()
            .filter(|(owner, _)| members.contains(owner))
            .map(|(_, key)| *key)
            .collect();
        let inner: Vec<FaceKey> = members.iter().map(|face| FaceKey(*face)).collect();
        let offset: Vec<FaceKey> = members.iter().map(|face| offset_face[*face]).collect();
        if walls.is_empty() {
            let encloses = |faces: &[FaceKey]| {
                let mut probe = out.clone();
                probe.shells = vec![Record {
                    id: crate::topology::EntityId::from_raw(1_000_000),
                    value: Shell {
                        faces: faces.to_vec(),
                    },
                }];
                probe.solids = vec![Record {
                    id: crate::topology::EntityId::from_raw(1_000_001),
                    value: Solid {
                        outer_shell: ShellKey(0),
                        inner_shells: Vec::new(),
                    },
                }];
                crate::validator::calculate_exact_shell_measures(&probe, None).is_some()
            };
            let (outer, cavity) = if encloses(&offset) {
                (offset, inner)
            } else {
                (inner, offset)
            };
            let outer_key = ShellKey(out.shells.len());
            out.shells.push(Record {
                id: allocate_id(&mut next_id),
                value: Shell { faces: outer },
            });
            let cavity_key = ShellKey(out.shells.len());
            out.shells.push(Record {
                id: allocate_id(&mut next_id),
                value: Shell { faces: cavity },
            });
            out.solids.push(Record {
                id: allocate_id(&mut next_id),
                value: Solid {
                    outer_shell: outer_key,
                    inner_shells: vec![cavity_key],
                },
            });
            continue;
        }
        let mut faces = inner;
        faces.extend(offset);
        faces.extend(walls);
        let shell_key = ShellKey(out.shells.len());
        out.shells.push(Record {
            id: allocate_id(&mut next_id),
            value: Shell { faces },
        });
        out.solids.push(Record {
            id: allocate_id(&mut next_id),
            value: Solid {
                outer_shell: shell_key,
                inner_shells: Vec::new(),
            },
        });
    }
    Ok(out)
}

/// The carrier `distance` along the face's outward normal from `surface`,
/// in the same parameterisation.
fn offset_surface(surface: Surface, distance: f64, minimum: f64) -> Result<Surface, ThickenError> {
    let sign = |radial_u: Vector3, radial_v: Vector3, axis: Vector3, angular_sign: f64| {
        let axis = unit(axis).ok_or(ThickenError::FaceUnsupported)?;
        frame_orientation(radial_u, radial_v, axis, angular_sign).ok_or(ThickenError::FaceUnsupported)
    };
    Ok(match surface {
        Surface::Plane(plane) => {
            let normal = unit(plane.normal).ok_or(ThickenError::FaceUnsupported)?;
            Surface::Plane(Plane::new(
                plane.origin + normal * distance,
                plane.u,
                plane.v,
            ))
        }
        Surface::Cylinder(cylinder) => {
            let sign = sign(
                cylinder.radial_u,
                cylinder.radial_v,
                cylinder.axis,
                cylinder.angular_sign,
            )?;
            let radius = cylinder.radius + sign * distance;
            if radius <= minimum {
                return Err(ThickenError::OffsetDegenerate);
            }
            Surface::Cylinder(Cylinder { radius, ..cylinder })
        }
        Surface::Cone(cone) => {
            let sign = sign(cone.radial_u, cone.radial_v, cone.axis, cone.angular_sign)?;
            let axis_length = cone.axis.length();
            // The normal is `(radial − slope·axis)/√(1 + slope²)` per unit
            // axis; moving along it by `d` grows every ring by
            // `d/√(1 + s²)` and slides the origin `d·s/√(1 + s²)` back.
            let slope = cone.slope / axis_length;
            let scale = (1.0 + slope * slope).sqrt();
            let radial = sign * distance / scale;
            let axial = -sign * distance * slope / scale;
            let base_radius = cone.base_radius + radial;
            Surface::Cone(Cone {
                origin: cone.origin + cone.axis * (axial / axis_length),
                base_radius,
                ..cone
            })
        }
        Surface::Sphere(sphere) => {
            let sign = sign(
                sphere.radial_u,
                sphere.radial_v,
                sphere.axis,
                sphere.angular_sign,
            )?;
            let radius = sphere.radius + sign * distance;
            if radius <= minimum {
                return Err(ThickenError::OffsetDegenerate);
            }
            Surface::Sphere(crate::topology::Sphere { radius, ..sphere })
        }
        Surface::Torus(torus) => {
            let sign = sign(
                torus.radial_u,
                torus.radial_v,
                torus.axis,
                torus.angular_sign,
            )?;
            let minor_radius = torus.minor_radius + sign * distance;
            if minor_radius <= minimum || minor_radius >= torus.major_radius - minimum {
                return Err(ThickenError::OffsetDegenerate);
            }
            Surface::Torus(crate::topology::Torus {
                minor_radius,
                ..torus
            })
        }
        Surface::Ruled(_) | Surface::Bspline(_) => return Err(ThickenError::FaceUnsupported),
    })
}

/// An edge carried along the normal: a line to the line between its
/// offset ends, a circle to the circle through its offset points at the
/// same parameters.
fn offset_curve(
    edge: &Edge,
    offset: impl Fn(Point3) -> Option<Point3>,
) -> Result<(Curve3, ParameterRange), ThickenError> {
    let range = edge.parameter_range;
    match edge.curve {
        Curve3::Line { endpoints } => {
            let moved = [
                offset(endpoints[0]).ok_or(ThickenError::VertexUnsupported)?,
                offset(endpoints[1]).ok_or(ThickenError::VertexUnsupported)?,
            ];
            Ok((Curve3::Line { endpoints: moved }, range))
        }
        Curve3::Circle { .. } => {
            let quarter = std::f64::consts::FRAC_PI_2;
            let at = |t: f64| offset(edge.curve.evaluate(t)).ok_or(ThickenError::EdgeUnsupported);
            let first = at(range.start)?;
            let second = at(range.start + quarter)?;
            let third = at(range.start + 2.0 * quarter)?;
            let center = Point3::new(
                (first.x + third.x) / 2.0,
                (first.y + third.y) / 2.0,
                (first.z + third.z) / 2.0,
            );
            let radius = first.distance(center);
            let u = unit(first - center).ok_or(ThickenError::EdgeUnsupported)?;
            let v = unit(second - center).ok_or(ThickenError::EdgeUnsupported)?;
            let scale = radius.max(1.0);
            if (second.distance(center) - radius).abs() > 1.0e-9 * scale
                || u.dot(v).abs() > 1.0e-9
            {
                return Err(ThickenError::EdgeUnsupported);
            }
            // The offset circle keeps the parameter: its point at the
            // start is the offset of the edge's, by construction. The
            // frame is read off the same points, so it is the circle's
            // own frame turned to those points, exactly.
            let curve = Curve3::Circle {
                center,
                u: u * range.start.cos() - v * range.start.sin(),
                v: u * range.start.sin() + v * range.start.cos(),
                radius,
            };
            Ok((curve, range))
        }
        Curve3::Ellipse { .. } | Curve3::Trace { .. } | Curve3::Bspline { .. } => {
            Err(ThickenError::EdgeUnsupported)
        }
    }
}

/// A wall's carrier and the pcurves of its four sides, in loop order:
/// along the boundary edge in the sheet's own direction, up the rung at
/// its end, back along the offset edge, and down the rung at its start.
struct Wall {
    surface: Surface,
    along: (Curve2, ParameterRange),
    rung_end: (Curve2, ParameterRange),
    back: (Curve2, ParameterRange),
    rung_start: (Curve2, ParameterRange),
}

fn wall_surface(
    face: &Face,
    edge: &Edge,
    orientation: Orientation,
    distance: f64,
) -> Result<Wall, ThickenError> {
    let range = edge.parameter_range;
    // The edge's parameter at the loop's start and end of it.
    let (from, to) = match orientation {
        Orientation::Forward => (range.start, range.end),
        Orientation::Reverse => (range.end, range.start),
    };
    let normal_at = |t: f64| {
        sheet::face_normal_at(face, edge.curve.evaluate(t)).ok_or(ThickenError::VertexUnsupported)
    };
    let stations = [from, (from + to) / 2.0, to];
    match edge.curve {
        Curve3::Line { .. } => {
            let start = edge.curve.evaluate(from);
            let end = edge.curve.evaluate(to);
            let length = start.distance(end);
            let tangent = unit(end - start).ok_or(ThickenError::EdgeUnsupported)?;
            let normal = normal_at(from)?;
            for station in stations {
                if normal_at(station)?.dot(normal) < 1.0 - PARALLEL {
                    return Err(ThickenError::EdgeUnsupported);
                }
            }
            let line = |a: Point2, b: Point2| Curve2::line_segment([a, b]);
            Ok(Wall {
                surface: Surface::Plane(Plane::new(start, tangent, normal)),
                along: line(Point2::new(0.0, 0.0), Point2::new(length, 0.0)),
                rung_end: line(Point2::new(length, 0.0), Point2::new(length, distance)),
                back: line(Point2::new(length, distance), Point2::new(0.0, distance)),
                rung_start: line(Point2::new(0.0, distance), Point2::new(0.0, 0.0)),
            })
        }
        Curve3::Circle {
            center,
            u,
            v,
            radius,
        } => {
            let axis = unit(u.cross(v)).ok_or(ThickenError::EdgeUnsupported)?;
            let direction = (to - from).signum();
            // The normal along the circle, split into what lies along the
            // circle's axis and what lies along its radius, which must be
            // the same all the way round for the wall to be one carrier.
            let components = |t: f64| -> Result<(f64, f64), ThickenError> {
                let point = edge.curve.evaluate(t);
                let radial = unit(point - center).ok_or(ThickenError::EdgeUnsupported)?;
                let normal = normal_at(t)?;
                let axial = normal.dot(axis);
                let outward = normal.dot(radial);
                if (axial * axial + outward * outward - 1.0).abs() > 1.0e-6 {
                    return Err(ThickenError::EdgeUnsupported);
                }
                Ok((axial, outward))
            };
            let (axial, outward) = components(from)?;
            for station in stations {
                let (a, o) = components(station)?;
                if (a - axial).abs() > 1.0e-6 || (o - outward).abs() > 1.0e-6 {
                    return Err(ThickenError::EdgeUnsupported);
                }
            }
            let line = |a: Point2, b: Point2| Curve2::line_segment([a, b]);
            if axial.abs() <= PARALLEL {
                // In the circle's own plane: an annulus sector.
                let sign = outward.signum();
                let swap = -direction * sign < 0.0;
                let surface = if swap {
                    Plane::new(center, v, u)
                } else {
                    Plane::new(center, u, v)
                };
                let (pu, pv) = if swap {
                    (Vector2::new(0.0, 1.0), Vector2::new(1.0, 0.0))
                } else {
                    (Vector2::new(1.0, 0.0), Vector2::new(0.0, 1.0))
                };
                let far = radius + outward * distance;
                if far <= 0.0 {
                    return Err(ThickenError::OffsetDegenerate);
                }
                let at = |rho: f64, t: f64| {
                    let (sin, cos) = t.sin_cos();
                    if swap {
                        Point2::new(rho * sin, rho * cos)
                    } else {
                        Point2::new(rho * cos, rho * sin)
                    }
                };
                let arc = |rho: f64, a: f64, b: f64| {
                    (
                        Curve2::Circle {
                            center: Point2::new(0.0, 0.0),
                            u: pu,
                            v: pv,
                            radius: rho,
                        },
                        ParameterRange::new(a, b),
                    )
                };
                Ok(Wall {
                    surface: Surface::Plane(surface),
                    along: arc(radius, from, to),
                    rung_end: line(at(radius, to), at(far, to)),
                    back: arc(far, to, from),
                    rung_start: line(at(far, from), at(radius, from)),
                })
            } else {
                let angular_sign = direction * axial.signum();
                let height = distance * axial;
                let surface = if outward.abs() <= PARALLEL {
                    Surface::Cylinder(Cylinder {
                        origin: center,
                        axis,
                        radial_u: u,
                        radial_v: v,
                        radius,
                        angular_sign,
                    })
                } else {
                    Surface::Cone(Cone {
                        origin: center,
                        axis,
                        radial_u: u,
                        radial_v: v,
                        base_radius: radius,
                        slope: outward / axial,
                        angular_sign,
                    })
                };
                let (x0, x1) = (angular_sign * from, angular_sign * to);
                Ok(Wall {
                    surface,
                    along: line(Point2::new(x0, 0.0), Point2::new(x1, 0.0)),
                    rung_end: line(Point2::new(x1, 0.0), Point2::new(x1, height)),
                    back: line(Point2::new(x1, height), Point2::new(x0, height)),
                    rung_start: line(Point2::new(x0, height), Point2::new(x0, 0.0)),
                })
            }
        }
        Curve3::Ellipse { .. } | Curve3::Trace { .. } | Curve3::Bspline { .. } => {
            Err(ThickenError::EdgeUnsupported)
        }
    }
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > f64::EPSILON).then(|| vector / length)
}

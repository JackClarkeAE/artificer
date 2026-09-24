//! Trim by a plane (ADR 0056, S2): a sheet cut along a plane's section
//! curves, keeping the side the plane's normal faces.
//!
//! On every face the plane's section is read in the face's own parameter
//! space, where it is a curve the planar Boolean already carries: a line on
//! a plane; on a cylinder a harmonic for an oblique plane, a ring for a
//! plane square to the axis, and two generators for one parallel to it; on
//! a cone, a sphere or a torus a ring for a plane square to the axis and
//! two meridians for one through it. The face's loops are then intersected
//! with the region on the kept side of that curve by the exact profile
//! Boolean, and the pieces are sewn back into a sheet on the same carriers.
//! An oblique section through a cone, a sphere or a torus is a curve this
//! kernel has no exact name for, and is refused by name.

use std::f64::consts::TAU;

use artificer_protocol::{
    BooleanOperation, ExecuteRequest, KernelError, KernelErrorCode, Point3 as ProtocolPoint3,
    PrecisionPolicy, SnapshotId, Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::{Segment, topology_loop_chords};
use crate::profile_boolean::{ProfileBooleanError, ProfileRegion, profile_boolean_multi};
use crate::sheet::{self, SheetResult};
use crate::sheet_sew::{SheetPiece, sew_pieces};
use crate::surface_intersection::{IntersectionError, SurfaceIntersection, intersect};
use crate::topology::{Face, Plane, Point2, Point3, Surface, Topology, Vector3};
use crate::{CancellationToken, ExecutionOutcome, Snapshot};

/// Why a trim was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrimError {
    PlaneInvalid,
    FaceUnsupported,
    SectionUnsupported,
    SectionIndeterminate,
    Empty,
}

impl TrimError {
    fn refuse(self, snapshot: SnapshotId) -> KernelError {
        let (code, name, message) = match self {
            Self::PlaneInvalid => (
                KernelErrorCode::InvalidInput,
                "TRIM_PLANE_INVALID",
                "A trim plane needs a finite origin and a finite, non-zero normal.",
            ),
            Self::FaceUnsupported => (
                KernelErrorCode::Unsupported,
                "TRIM_FACE_UNSUPPORTED",
                "Trim cuts faces on planes, cylinders, cones, spheres and tori bounded by lines, arcs, ellipses and plane sections; a ruled or B-spline face, or one bounded by a cylinder trace, is not cut in this release.",
            ),
            Self::SectionUnsupported => (
                KernelErrorCode::Unsupported,
                "TRIM_SECTION_UNSUPPORTED",
                "The plane meets a cone, a sphere or a torus obliquely, in a curve this kernel has no exact name for; a plane square to the axis or through it is cut exactly.",
            ),
            Self::SectionIndeterminate => (
                KernelErrorCode::NumericallyIndeterminate,
                "TRIM_SECTION_INDETERMINATE",
                "The plane's section grazes a face's boundary within the minimum feature size, or runs along it, and the split could not be certified; move the plane a little.",
            ),
            Self::Empty => (
                KernelErrorCode::InvalidInput,
                "TRIM_RESULT_EMPTY",
                "Nothing of the sheet lies on the kept side of the plane.",
            ),
        };
        sheet::refuse(snapshot, code, name, message)
    }
}

/// `KernelCommand::TrimSheetByPlane`.
pub(crate) fn execute_trim(
    input: &Snapshot,
    request: &ExecuteRequest,
    cancellation: &CancellationToken,
    plane_origin: ProtocolPoint3,
    plane_normal: ProtocolVector3,
) -> Result<ExecutionOutcome, KernelError> {
    if !sheet::is_sheet(&input.topology) {
        return Err(sheet::not_a_sheet(input.id, "Trim"));
    }
    let origin = Point3::new(plane_origin.x, plane_origin.y, plane_origin.z);
    let normal = Vector3::new(plane_normal.x, plane_normal.y, plane_normal.z);
    let normal = unit(normal)
        .filter(|_| origin.is_finite())
        .ok_or_else(|| TrimError::PlaneInvalid.refuse(input.id))?;
    let cut = Cut::new(origin, normal);
    let mut pieces = Vec::new();
    for face in &input.topology.faces {
        pieces.extend(
            trim_face(&input.topology, &face.value, &cut, request.precision)
                .map_err(|reason| reason.refuse(input.id))?,
        );
    }
    if pieces.is_empty() {
        return Err(TrimError::Empty.refuse(input.id));
    }
    let topology = sew_pieces(&pieces, request.precision).map_err(|_| {
        sheet::refuse(
            input.id,
            KernelErrorCode::InternalFailure,
            "TRIM_SEW_FAILED",
            "The trimmed pieces could not be sewn back into a sheet.",
        )
    })?;
    sheet::commit(
        input,
        request,
        cancellation,
        SheetResult {
            topology,
            rung: sheet::TRIM_RUNG,
            warnings: Vec::new(),
        },
    )
}

/// The trim plane, with the side it keeps.
struct Cut {
    origin: Point3,
    normal: Vector3,
    plane: Plane,
}

impl Cut {
    fn new(origin: Point3, normal: Vector3) -> Self {
        let seed = if normal.x.abs() < 0.9 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let u = unit(seed - normal * seed.dot(normal)).unwrap_or(seed);
        let v = normal.cross(u);
        Self {
            origin,
            normal,
            plane: Plane::new(origin, u, v),
        }
    }

    /// The signed distance of a point from the plane, positive on the
    /// kept side.
    fn signed(&self, point: Point3) -> f64 {
        (point - self.origin).dot(self.normal)
    }

    fn keeps(&self, surface: Surface, point: Point2) -> bool {
        self.signed(surface.evaluate(point)) >= 0.0
    }
}

/// Where a face's parameter space is divided by the section, and which
/// side of it is kept.
enum Divider {
    /// `a·u + b·v + c ≥ 0` is kept: a plane face.
    HalfPlane { a: f64, b: f64, c: f64 },
    /// Lines `u = value`, between which the kept side is sampled.
    Vertical(Vec<f64>),
    /// Lines `v = value`, likewise.
    Horizontal(Vec<f64>),
    /// `v = mean + amplitude·cos(u − phase)`, kept above or below.
    Harmonic {
        mean: f64,
        amplitude: f64,
        phase: f64,
        above: bool,
    },
}

/// The pieces of one face on the kept side of the plane.
fn trim_face(
    topology: &Topology,
    face: &Face,
    cut: &Cut,
    precision: PrecisionPolicy,
) -> Result<Vec<SheetPiece>, TrimError> {
    let loops: Vec<Vec<Segment>> = face
        .loops()
        .map(|loop_key| topology_loop_chords(topology, loop_key))
        .collect::<Option<_>>()
        .ok_or(TrimError::FaceUnsupported)?;
    if loops
        .iter()
        .flatten()
        .any(|segment| matches!(segment, Segment::Trace { .. }))
    {
        return Err(TrimError::FaceUnsupported);
    }
    let whole = |keep: bool| -> Vec<SheetPiece> {
        if keep {
            vec![SheetPiece {
                surface: face.surface,
                loops: loops.clone(),
                role: face.role,
            }]
        } else {
            Vec::new()
        }
    };
    let sample = loops[0][0].start();
    let intersection = match intersect(Surface::Plane(cut.plane), face.surface, precision) {
        Ok(intersection) => intersection,
        Err(IntersectionError::Unsupported | IntersectionError::Indeterminate) => {
            return Err(TrimError::FaceUnsupported);
        }
    };
    match intersection {
        SurfaceIntersection::Empty => return Ok(whole(cut.keeps(face.surface, sample))),
        SurfaceIntersection::Coincident => return Ok(whole(true)),
        SurfaceIntersection::Curves(_) => {}
    }
    let (u_min, u_max, v_min, v_max) =
        crate::validator::pcurve_extent(topology, face).ok_or(TrimError::FaceUnsupported)?;
    let margin_u = (0.5 * (u_max - u_min)).max(1.0e-3);
    let margin_v = (0.5 * (v_max - v_min)).max(1.0e-3);
    let window = [
        u_min - margin_u,
        u_max + margin_u,
        v_min - margin_v,
        v_max + margin_v,
    ];
    let scale = [
        u_min.abs(),
        u_max.abs(),
        v_min.abs(),
        v_max.abs(),
        face_scale(face.surface),
    ]
    .into_iter()
    .fold(1.0_f64, f64::max);
    let Some(divider) = divider(face.surface, cut, window, precision, scale)? else {
        return Ok(whole(cut.keeps(face.surface, sample)));
    };
    let face_region = ProfileRegion {
        outer: loops[0].clone(),
        holes: loops[1..].to_vec(),
    };
    let tools = tool_regions(face.surface, cut, &divider, window);
    let mut pieces = Vec::new();
    for tool in tools {
        match tool {
            Tool::Whole => return Ok(whole(true)),
            Tool::Region(outer) => {
                let tool = ProfileRegion {
                    outer,
                    holes: Vec::new(),
                };
                match profile_boolean_multi(
                    std::slice::from_ref(&face_region),
                    std::slice::from_ref(&tool),
                    BooleanOperation::Intersection,
                    precision,
                ) {
                    Ok(regions) => {
                        for region in regions {
                            let mut loops = vec![region.outer];
                            loops.extend(region.holes);
                            pieces.push(SheetPiece {
                                surface: face.surface,
                                loops,
                                role: face.role,
                            });
                        }
                    }
                    Err(ProfileBooleanError::EmptyResult) => {}
                    Err(ProfileBooleanError::Unsupported) => {
                        return Err(TrimError::SectionIndeterminate);
                    }
                }
            }
        }
    }
    Ok(pieces)
}

/// A length the carrier is the size of, for tolerances.
fn face_scale(surface: Surface) -> f64 {
    let origin = |point: Point3| point.x.abs().max(point.y.abs()).max(point.z.abs());
    match surface {
        Surface::Plane(plane) => origin(plane.origin),
        Surface::Cylinder(cylinder) => origin(cylinder.origin).max(cylinder.radius.abs()),
        Surface::Cone(cone) => origin(cone.origin).max(cone.base_radius.abs()),
        Surface::Sphere(sphere) => origin(sphere.origin).max(sphere.radius.abs()),
        Surface::Torus(torus) => origin(torus.origin).max(torus.major_radius.abs()),
        Surface::Ruled(_) | Surface::Bspline(_) => 1.0,
    }
}

/// The section of the plane in the face's parameter space, or `None`
/// when the plane clears the carrier over the face's window.
fn divider(
    surface: Surface,
    cut: &Cut,
    window: [f64; 4],
    precision: PrecisionPolicy,
    scale: f64,
) -> Result<Option<Divider>, TrimError> {
    let angular = precision.angular_agreement_radians.max(1.0e-9);
    let linear = crate::sheet_sew::weld_distance(precision, scale);
    let n = cut.normal;
    // The plane against a carrier of revolution about `axis` with radial
    // frame `(ru, rv)` and origin `c`: `(P − o)·n = C + ρ·m·cos(θ − φ) +
    // na·z` for a point at radius `ρ` and height `z`.
    let revolved = |c: Point3, axis: Vector3, ru: Vector3, rv: Vector3| {
        let axis = unit(axis).unwrap_or(axis);
        let na = n.dot(axis);
        let (cu, cv) = (n.dot(ru), n.dot(rv));
        let m = cu.hypot(cv);
        let phi = cv.atan2(cu);
        let constant = (c - cut.origin).dot(n);
        (na, m, phi, constant)
    };
    // The angles `θ = φ ± acos(k)`, as face parameters within the window.
    let verticals = |angular_sign: f64, phi: f64, k: f64| -> Vec<f64> {
        if k.abs() > 1.0 {
            return Vec::new();
        }
        let spread = k.clamp(-1.0, 1.0).acos();
        let mut values = Vec::new();
        for theta in [phi - spread, phi + spread] {
            let x = angular_sign * theta;
            let first = ((window[0] - x) / TAU).floor() as i64;
            let last = ((window[1] - x) / TAU).ceil() as i64;
            for turn in first..=last {
                let value = (turn as f64).mul_add(TAU, x);
                if value >= window[0] && value <= window[1] {
                    values.push(value);
                }
            }
        }
        values
    };
    let horizontals = |values: Vec<f64>| -> Vec<f64> {
        values
            .into_iter()
            .filter(|value| value.is_finite() && *value >= window[2] && *value <= window[3])
            .collect()
    };
    let non_empty = |values: Vec<f64>, make: fn(Vec<f64>) -> Divider| {
        if values.is_empty() {
            None
        } else {
            Some(make(values))
        }
    };
    Ok(match surface {
        Surface::Plane(plane) => {
            let a = n.dot(plane.u);
            let b = n.dot(plane.v);
            let c = (plane.origin - cut.origin).dot(n);
            if a.hypot(b) <= angular {
                None
            } else {
                Some(Divider::HalfPlane { a, b, c })
            }
        }
        Surface::Cylinder(cylinder) => {
            let (na, m, phi, constant) = revolved(
                cylinder.origin,
                cylinder.axis,
                cylinder.radial_u,
                cylinder.radial_v,
            );
            if na.abs() <= angular {
                // Parallel to the axis: two generators.
                let k = -constant / (cylinder.radius * m);
                non_empty(verticals(cylinder.angular_sign, phi, k), Divider::Vertical)
            } else if m <= angular {
                non_empty(horizontals(vec![-constant / na]), Divider::Horizontal)
            } else {
                let mean = -constant / na;
                let mut amplitude = -cylinder.radius * m / na;
                let mut phase = cylinder.angular_sign * phi;
                if amplitude < 0.0 {
                    amplitude = -amplitude;
                    phase += std::f64::consts::PI;
                }
                Some(Divider::Harmonic {
                    mean,
                    amplitude,
                    phase,
                    above: na > 0.0,
                })
            }
        }
        Surface::Cone(cone) => {
            let (na, m, phi, constant) =
                revolved(cone.origin, cone.axis, cone.radial_u, cone.radial_v);
            if m <= angular {
                non_empty(horizontals(vec![-constant / na]), Divider::Horizontal)
            } else if na.abs() <= angular && constant.abs() <= linear {
                non_empty(verticals(cone.angular_sign, phi, 0.0), Divider::Vertical)
            } else {
                return Err(TrimError::SectionUnsupported);
            }
        }
        Surface::Sphere(sphere) => {
            let (na, m, phi, constant) =
                revolved(sphere.origin, sphere.axis, sphere.radial_u, sphere.radial_v);
            if m <= angular {
                let k = -constant / (sphere.radius * na);
                if k.abs() > 1.0 {
                    None
                } else {
                    non_empty(horizontals(vec![k.asin()]), Divider::Horizontal)
                }
            } else if na.abs() <= angular && constant.abs() <= linear {
                non_empty(verticals(sphere.angular_sign, phi, 0.0), Divider::Vertical)
            } else {
                return Err(TrimError::SectionUnsupported);
            }
        }
        Surface::Torus(torus) => {
            let (na, m, phi, constant) =
                revolved(torus.origin, torus.axis, torus.radial_u, torus.radial_v);
            if m <= angular {
                let k = -constant / (torus.minor_radius * na);
                if k.abs() > 1.0 {
                    None
                } else {
                    let latitude = k.asin();
                    let mut values = Vec::new();
                    for base in [latitude, std::f64::consts::PI - latitude] {
                        let first = ((window[2] - base) / TAU).floor() as i64;
                        let last = ((window[3] - base) / TAU).ceil() as i64;
                        for turn in first..=last {
                            values.push((turn as f64).mul_add(TAU, base));
                        }
                    }
                    non_empty(horizontals(values), Divider::Horizontal)
                }
            } else if na.abs() <= angular && constant.abs() <= linear {
                non_empty(verticals(torus.angular_sign, phi, 0.0), Divider::Vertical)
            } else {
                return Err(TrimError::SectionUnsupported);
            }
        }
        Surface::Ruled(_) | Surface::Bspline(_) => return Err(TrimError::FaceUnsupported),
    })
}

/// A region to intersect the face with, or the whole window.
enum Tool {
    Whole,
    Region(Vec<Segment>),
}

/// The regions of the window on the kept side of the divider, each a loop
/// of exact segments.
fn tool_regions(surface: Surface, cut: &Cut, divider: &Divider, window: [f64; 4]) -> Vec<Tool> {
    let [u_lo, u_hi, v_lo, v_hi] = window;
    let line = |start: Point2, end: Point2| Segment::Line { start, end };
    let rectangle = |a: f64, b: f64, c: f64, d: f64| -> Vec<Segment> {
        vec![
            line(Point2::new(a, c), Point2::new(b, c)),
            line(Point2::new(b, c), Point2::new(b, d)),
            line(Point2::new(b, d), Point2::new(a, d)),
            line(Point2::new(a, d), Point2::new(a, c)),
        ]
    };
    // Strips between consecutive dividers along one parameter, merged
    // where neighbours are both kept.
    let strips = |values: &[f64], vertical: bool| -> Vec<Tool> {
        let (lo, hi, other) = if vertical {
            (u_lo, u_hi, (v_lo + v_hi) / 2.0)
        } else {
            (v_lo, v_hi, (u_lo + u_hi) / 2.0)
        };
        let mut breaks = vec![lo];
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        for value in sorted {
            if value > lo && value < hi && breaks.last().is_none_or(|last| value - last > 1.0e-12) {
                breaks.push(value);
            }
        }
        breaks.push(hi);
        let kept: Vec<bool> = breaks
            .windows(2)
            .map(|pair| {
                let middle = (pair[0] + pair[1]) / 2.0;
                let point = if vertical {
                    Point2::new(middle, other)
                } else {
                    Point2::new(other, middle)
                };
                cut.keeps(surface, point)
            })
            .collect();
        if kept.iter().all(|keep| *keep) {
            return vec![Tool::Whole];
        }
        let mut tools = Vec::new();
        let mut index = 0;
        while index < kept.len() {
            if !kept[index] {
                index += 1;
                continue;
            }
            let start = breaks[index];
            let mut end_index = index;
            while end_index + 1 < kept.len() && kept[end_index + 1] {
                end_index += 1;
            }
            let end = breaks[end_index + 1];
            tools.push(Tool::Region(if vertical {
                rectangle(start, end, v_lo, v_hi)
            } else {
                rectangle(u_lo, u_hi, start, end)
            }));
            index = end_index + 1;
        }
        tools
    };
    match divider {
        Divider::HalfPlane { a, b, c } => {
            let corners = [
                Point2::new(u_lo, v_lo),
                Point2::new(u_hi, v_lo),
                Point2::new(u_hi, v_hi),
                Point2::new(u_lo, v_hi),
            ];
            let value = |point: Point2| a * point.x + b * point.y + c;
            if corners.iter().all(|corner| value(*corner) >= 0.0) {
                return vec![Tool::Whole];
            }
            let mut polygon: Vec<Point2> = Vec::new();
            for index in 0..4 {
                let p = corners[index];
                let q = corners[(index + 1) % 4];
                let (sp, sq) = (value(p), value(q));
                if sp >= 0.0 {
                    polygon.push(p);
                }
                if (sp < 0.0 && sq > 0.0) || (sp > 0.0 && sq < 0.0) {
                    let t = sp / (sp - sq);
                    polygon.push(Point2::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t));
                }
            }
            if polygon.len() < 3 {
                return Vec::new();
            }
            let count = polygon.len();
            vec![Tool::Region(
                (0..count)
                    .map(|index| line(polygon[index], polygon[(index + 1) % count]))
                    .collect(),
            )]
        }
        Divider::Vertical(values) => strips(values, true),
        Divider::Horizontal(values) => strips(values, false),
        Divider::Harmonic {
            mean,
            amplitude,
            phase,
            above,
        } => {
            let height = |x: f64| mean + amplitude * (x - phase).cos();
            let (start, end) = (
                Point2::new(u_lo, height(u_lo)),
                Point2::new(u_hi, height(u_hi)),
            );
            let harmonic = Segment::Harmonic {
                mean: *mean,
                amplitude: *amplitude,
                phase: *phase,
                start,
                end,
            };
            let far = if *above {
                v_hi.max(mean + amplitude.abs()) + (v_hi - v_lo)
            } else {
                v_lo.min(mean - amplitude.abs()) - (v_hi - v_lo)
            };
            vec![Tool::Region(vec![
                harmonic,
                line(end, Point2::new(u_hi, far)),
                line(Point2::new(u_hi, far), Point2::new(u_lo, far)),
                line(Point2::new(u_lo, far), start),
            ])]
        }
    }
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > f64::EPSILON).then(|| vector / length)
}

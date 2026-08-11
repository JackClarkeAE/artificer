use std::fmt;

pub(crate) use artificer_geometry::{Point2, Vector2};

/// Snapshot-local, public-facing identity for a topological entity.
///
/// The value is deliberately opaque outside this crate. It is deterministic for
/// the same command replay, but semantic comparisons must never depend on it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(u64);

impl EntityId {
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for EntityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "EntityId({})", self.0)
    }
}

macro_rules! storage_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub(crate) struct $name(pub(crate) usize);
    };
}

storage_id!(VertexKey);
storage_id!(EdgeKey);
storage_id!(CoedgeKey);
storage_id!(LoopKey);
storage_id!(FaceKey);
storage_id!(ShellKey);
storage_id!(SolidKey);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Point3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub(crate) fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    pub(crate) fn distance(self, other: Self) -> f64 {
        (self - other).length()
    }

    pub(crate) fn as_vector(self) -> Vector3 {
        Vector3::new(self.x, self.y, self.z)
    }
}

impl std::ops::Add<Vector3> for Point3 {
    type Output = Self;

    fn add(self, rhs: Vector3) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub for Point3 {
    type Output = Vector3;

    fn sub(self, rhs: Self) -> Self::Output {
        Vector3::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vector3 {
    pub(crate) const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub(crate) fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    pub(crate) fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    pub(crate) fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    pub(crate) fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl std::ops::Add for Vector3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub for Vector3 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Mul<f64> for Vector3 {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl std::ops::Div<f64> for Vector3 {
    type Output = Self;

    fn div(self, rhs: f64) -> Self::Output {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plane {
    pub(crate) origin: Point3,
    pub(crate) u: Vector3,
    pub(crate) v: Vector3,
    pub(crate) normal: Vector3,
}

impl Plane {
    pub(crate) fn new(origin: Point3, u: Vector3, v: Vector3) -> Self {
        Self {
            origin,
            u,
            v,
            normal: u.cross(v),
        }
    }

    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        self.origin + self.u * point.x + self.v * point.y
    }

    pub(crate) fn project(self, point: Point3) -> Point2 {
        let relative = point - self.origin;
        let u_denominator = self.u.dot(self.u);
        let v_denominator = self.v.dot(self.v);
        Point2::new(
            relative.dot(self.u) / u_denominator,
            relative.dot(self.v) / v_denominator,
        )
    }

    pub(crate) fn is_finite(self) -> bool {
        self.origin.is_finite()
            && self.u.is_finite()
            && self.v.is_finite()
            && self.normal.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Orientation {
    Forward,
    Reverse,
}

impl Orientation {
    pub(crate) const fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Reverse,
            Self::Reverse => Self::Forward,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Stable construction/source role.
///
/// Axis names describe cuboid construction space, while extrusion roles retain
/// the generating profile relation and side ordinal. After a committed
/// rotation none of these roles claim a current world-axis direction.
pub enum FaceRole {
    NegativeX,
    PositiveX,
    NegativeY,
    PositiveY,
    NegativeZ,
    PositiveZ,
    ExtrusionBottom,
    ExtrusionTop,
    ExtrusionSide(u32),
    FeatureEnd,
    FeatureSide(u32),
}

#[derive(Clone, Debug)]
pub(crate) struct Record<T> {
    pub(crate) id: EntityId,
    pub(crate) value: T,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Vertex {
    pub(crate) point: Point3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ParameterRange {
    pub(crate) start: f64,
    pub(crate) end: f64,
}

impl ParameterRange {
    pub(crate) const fn new(start: f64, end: f64) -> Self {
        Self { start, end }
    }

    pub(crate) const fn reversed(self) -> Self {
        Self::new(self.end, self.start)
    }

    pub(crate) fn is_finite(self) -> bool {
        self.start.is_finite() && self.end.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Curve3 {
    Line {
        endpoints: [Point3; 2],
    },
    Circle {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
    },
}

impl Curve3 {
    pub(crate) fn line_segment(endpoints: [Point3; 2]) -> (Self, ParameterRange) {
        (Self::Line { endpoints }, ParameterRange::new(0.0, 1.0))
    }

    pub(crate) fn evaluate(self, parameter: f64) -> Point3 {
        match self {
            Self::Line { endpoints } => {
                if parameter == 0.0 {
                    endpoints[0]
                } else if parameter == 1.0 {
                    endpoints[1]
                } else {
                    endpoints[0] + (endpoints[1] - endpoints[0]) * parameter
                }
            }
            Self::Circle {
                center,
                u,
                v,
                radius,
            } => {
                // A circle's parameterization is periodic, so evaluating it
                // should be too. `sin(2π)` is not `sin(0)` in binary floating
                // point — it is about -2.4e-16 — so a full-turn parameter
                // lands a few femtometres off the point it names, and off the
                // seam vertex the topology stores there. Folding the
                // parameter into one turn first makes the wrap exact. Values
                // already inside a turn are returned unchanged, so nothing
                // else moves.
                let parameter = parameter.rem_euclid(std::f64::consts::TAU);
                center + u * (radius * parameter.cos()) + v * (radius * parameter.sin())
            }
        }
    }

    pub(crate) fn derivative(self, parameter: f64) -> Vector3 {
        match self {
            Self::Line { endpoints } => endpoints[1] - endpoints[0],
            Self::Circle { u, v, radius, .. } => {
                u * (-radius * parameter.sin()) + v * (radius * parameter.cos())
            }
        }
    }

    pub(crate) fn is_finite(self) -> bool {
        match self {
            Self::Line { endpoints } => endpoints.into_iter().all(Point3::is_finite),
            Self::Circle {
                center,
                u,
                v,
                radius,
            } => center.is_finite() && u.is_finite() && v.is_finite() && radius.is_finite(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Edge {
    pub(crate) vertices: [VertexKey; 2],
    pub(crate) curve: Curve3,
    pub(crate) parameter_range: ParameterRange,
}

impl Edge {
    pub(crate) fn line(vertices: [VertexKey; 2], endpoints: [Point3; 2]) -> Self {
        let (curve, parameter_range) = Curve3::line_segment(endpoints);
        Self {
            vertices,
            curve,
            parameter_range,
        }
    }

    pub(crate) fn endpoints(self) -> [Point3; 2] {
        [
            self.curve.evaluate(self.parameter_range.start),
            self.curve.evaluate(self.parameter_range.end),
        ]
    }

    pub(crate) fn length(self) -> f64 {
        match self.curve {
            Curve3::Line { endpoints } => endpoints[0].distance(endpoints[1]),
            Curve3::Circle { radius, .. } => {
                radius * (self.parameter_range.end - self.parameter_range.start).abs()
            }
        }
    }

    pub(crate) fn set_line_endpoints(&mut self, endpoints: [Point3; 2]) -> bool {
        if !matches!(self.curve, Curve3::Line { .. }) {
            return false;
        }
        let (curve, parameter_range) = Curve3::line_segment(endpoints);
        self.curve = curve;
        self.parameter_range = parameter_range;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Curve2 {
    Line {
        endpoints: [Point2; 2],
    },
    Circle {
        center: Point2,
        u: Vector2,
        v: Vector2,
        radius: f64,
    },
}

impl Curve2 {
    pub(crate) fn line_segment(endpoints: [Point2; 2]) -> (Self, ParameterRange) {
        // Surface parameters are not generally metric (cylinder `u` is an
        // angle), so retain the caller's exact endpoint interpolation on the
        // canonical [0, 1] range.
        (Self::Line { endpoints }, ParameterRange::new(0.0, 1.0))
    }

    pub(crate) fn evaluate(self, parameter: f64) -> Point2 {
        match self {
            Self::Line { endpoints } => {
                if parameter == 0.0 {
                    endpoints[0]
                } else if parameter == 1.0 {
                    endpoints[1]
                } else {
                    Point2::new(
                        endpoints[0].x + (endpoints[1].x - endpoints[0].x) * parameter,
                        endpoints[0].y + (endpoints[1].y - endpoints[0].y) * parameter,
                    )
                }
            }
            Self::Circle {
                center,
                u,
                v,
                radius,
            } => Point2::new(
                center.x + radius * (u.x * parameter.cos() + v.x * parameter.sin()),
                center.y + radius * (u.y * parameter.cos() + v.y * parameter.sin()),
            ),
        }
    }

    pub(crate) fn derivative(self, parameter: f64) -> Vector2 {
        match self {
            Self::Line { endpoints } => Vector2::new(
                endpoints[1].x - endpoints[0].x,
                endpoints[1].y - endpoints[0].y,
            ),
            Self::Circle { u, v, radius, .. } => Vector2::new(
                radius * (-u.x * parameter.sin() + v.x * parameter.cos()),
                radius * (-u.y * parameter.sin() + v.y * parameter.cos()),
            ),
        }
    }

    pub(crate) fn is_finite(self) -> bool {
        match self {
            Self::Line { endpoints } => endpoints.into_iter().all(Point2::is_finite),
            Self::Circle {
                center,
                u,
                v,
                radius,
            } => center.is_finite() && u.is_finite() && v.is_finite() && radius.is_finite(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Coedge {
    pub(crate) edge: EdgeKey,
    pub(crate) orientation: Orientation,
    pub(crate) pcurve: Curve2,
    pub(crate) parameter_range: ParameterRange,
}

impl Coedge {
    pub(crate) fn line(edge: EdgeKey, orientation: Orientation, endpoints: [Point2; 2]) -> Self {
        let (pcurve, parameter_range) = Curve2::line_segment(endpoints);
        Self {
            edge,
            orientation,
            pcurve,
            parameter_range,
        }
    }

    pub(crate) fn pcurve_endpoints(self) -> [Point2; 2] {
        [
            self.pcurve.evaluate(self.parameter_range.start),
            self.pcurve.evaluate(self.parameter_range.end),
        ]
    }

    pub(crate) fn set_line_pcurve_endpoints(&mut self, endpoints: [Point2; 2]) -> bool {
        if !matches!(self.pcurve, Curve2::Line { .. }) {
            return false;
        }
        let (pcurve, parameter_range) = Curve2::line_segment(endpoints);
        self.pcurve = pcurve;
        self.parameter_range = parameter_range;
        true
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Loop {
    pub(crate) coedges: Vec<CoedgeKey>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cylinder {
    pub(crate) origin: Point3,
    pub(crate) axis: Vector3,
    pub(crate) radial_u: Vector3,
    pub(crate) radial_v: Vector3,
    pub(crate) radius: f64,
    /// `+1` makes increasing `u` travel radial_u→radial_v; `-1`
    /// parameterizes the same cylinder oppositely for an inward-facing wall.
    pub(crate) angular_sign: f64,
}

impl Cylinder {
    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        let angle = self.angular_sign * point.x;
        self.origin
            + self.radial_u * (self.radius * angle.cos())
            + self.radial_v * (self.radius * angle.sin())
            + self.axis * point.y
    }

    pub(crate) fn is_finite(self) -> bool {
        self.origin.is_finite()
            && self.axis.is_finite()
            && self.radial_u.is_finite()
            && self.radial_v.is_finite()
            && self.radius.is_finite()
            && self.angular_sign.is_finite()
    }
}

/// Ring torus produced by an exact rim fillet (ADR 0023). Parameterized as
/// `u` = azimuth about `axis` (same sign convention as [`Cylinder`]) and
/// `v` = minor angle: `v = 0` lies on the outer equator (wall tangency) and
/// `v = π/2` on the top circle (cap tangency).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Torus {
    pub(crate) origin: Point3,
    pub(crate) axis: Vector3,
    pub(crate) radial_u: Vector3,
    pub(crate) radial_v: Vector3,
    pub(crate) major_radius: f64,
    pub(crate) minor_radius: f64,
    pub(crate) angular_sign: f64,
}

impl Torus {
    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        let angle = self.angular_sign * point.x;
        let radial = self.radial_u * angle.cos() + self.radial_v * angle.sin();
        let ring = self.major_radius + self.minor_radius * point.y.cos();
        self.origin + radial * ring + self.axis * (self.minor_radius * point.y.sin())
    }

    pub(crate) fn is_finite(self) -> bool {
        self.origin.is_finite()
            && self.axis.is_finite()
            && self.radial_u.is_finite()
            && self.radial_v.is_finite()
            && self.major_radius.is_finite()
            && self.minor_radius.is_finite()
            && self.angular_sign.is_finite()
    }
}

/// Cone frustum band produced by an exact rim chamfer (ADR 0023).
/// Parameterized as `u` = azimuth about `axis` (same sign convention as
/// [`Cylinder`]) and `v` = axial offset from `origin`'s plane, with the ring
/// radius varying linearly: `r(v) = base_radius + slope * v`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cone {
    pub(crate) origin: Point3,
    pub(crate) axis: Vector3,
    pub(crate) radial_u: Vector3,
    pub(crate) radial_v: Vector3,
    pub(crate) base_radius: f64,
    pub(crate) slope: f64,
    pub(crate) angular_sign: f64,
}

impl Cone {
    pub(crate) fn ring_radius(self, v: f64) -> f64 {
        self.base_radius + self.slope * v
    }

    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        let angle = self.angular_sign * point.x;
        let radial = self.radial_u * angle.cos() + self.radial_v * angle.sin();
        self.origin + radial * self.ring_radius(point.y) + self.axis * point.y
    }

    pub(crate) fn is_finite(self) -> bool {
        self.origin.is_finite()
            && self.axis.is_finite()
            && self.radial_u.is_finite()
            && self.radial_v.is_finite()
            && self.base_radius.is_finite()
            && self.slope.is_finite()
            && self.angular_sign.is_finite()
    }
}

/// Sphere patch closing the corner of a rim-loop blend (ADR 0023 frontier).
/// Parameterized as `u` = azimuth about `axis` (the [`Cylinder`] sign
/// convention) and `v` = latitude from the equator, so `v = +π/2` is the pole
/// toward `axis`. Both poles are parameter singularities: the `u` derivative
/// vanishes there, and any patch reaching a pole closes through a pole edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sphere {
    pub(crate) origin: Point3,
    pub(crate) axis: Vector3,
    pub(crate) radial_u: Vector3,
    pub(crate) radial_v: Vector3,
    pub(crate) radius: f64,
    pub(crate) angular_sign: f64,
}

impl Sphere {
    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        let angle = self.angular_sign * point.x;
        let radial = self.radial_u * angle.cos() + self.radial_v * angle.sin();
        self.origin
            + radial * (self.radius * point.y.cos())
            + self.axis * (self.radius * point.y.sin())
    }

    pub(crate) fn is_finite(self) -> bool {
        self.origin.is_finite()
            && self.axis.is_finite()
            && self.radial_u.is_finite()
            && self.radial_v.is_finite()
            && self.radius.is_finite()
            && self.angular_sign.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Surface {
    Plane(Plane),
    Cylinder(Cylinder),
    Torus(Torus),
    Cone(Cone),
    /// Corner patch of a rim-loop blend (ADR 0023 frontier, milestone B).
    /// Validation, transforms, hashing, measures, and tessellation all handle
    /// it; the builder that emits one is still to come.
    #[allow(dead_code)]
    Sphere(Sphere),
}

impl Surface {
    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        match self {
            Self::Plane(plane) => plane.evaluate(point),
            Self::Cylinder(cylinder) => cylinder.evaluate(point),
            Self::Torus(torus) => torus.evaluate(point),
            Self::Cone(cone) => cone.evaluate(point),
            Self::Sphere(sphere) => sphere.evaluate(point),
        }
    }

    pub(crate) fn is_finite(self) -> bool {
        match self {
            Self::Plane(plane) => plane.is_finite(),
            Self::Cylinder(cylinder) => cylinder.is_finite(),
            Self::Torus(torus) => torus.is_finite(),
            Self::Cone(cone) => cone.is_finite(),
            Self::Sphere(sphere) => sphere.is_finite(),
        }
    }

    /// Maps a parameter-space tangent through the exact surface differential.
    pub(crate) fn map_tangent(self, point: Point2, tangent: Vector2) -> Vector3 {
        match self {
            Self::Plane(plane) => plane.u * tangent.x + plane.v * tangent.y,
            Self::Cylinder(cylinder) => {
                let angle = cylinder.angular_sign * point.x;
                let angular = cylinder.radial_u * (-cylinder.radius * angle.sin())
                    + cylinder.radial_v * (cylinder.radius * angle.cos());
                angular * (cylinder.angular_sign * tangent.x) + cylinder.axis * tangent.y
            }
            Self::Torus(torus) => {
                let angle = torus.angular_sign * point.x;
                let radial = torus.radial_u * angle.cos() + torus.radial_v * angle.sin();
                let radial_derivative =
                    torus.radial_u * -angle.sin() + torus.radial_v * angle.cos();
                let ring = torus.major_radius + torus.minor_radius * point.y.cos();
                let azimuthal = radial_derivative * (ring * torus.angular_sign);
                let minor = radial * (-torus.minor_radius * point.y.sin())
                    + torus.axis * (torus.minor_radius * point.y.cos());
                azimuthal * tangent.x + minor * tangent.y
            }
            Self::Cone(cone) => {
                let angle = cone.angular_sign * point.x;
                let radial = cone.radial_u * angle.cos() + cone.radial_v * angle.sin();
                let radial_derivative = cone.radial_u * -angle.sin() + cone.radial_v * angle.cos();
                let azimuthal = radial_derivative * (cone.ring_radius(point.y) * cone.angular_sign);
                let axial = radial * cone.slope + cone.axis;
                azimuthal * tangent.x + axial * tangent.y
            }
            Self::Sphere(sphere) => {
                let angle = sphere.angular_sign * point.x;
                let radial = sphere.radial_u * angle.cos() + sphere.radial_v * angle.sin();
                let radial_derivative =
                    sphere.radial_u * -angle.sin() + sphere.radial_v * angle.cos();
                let azimuthal =
                    radial_derivative * (sphere.radius * point.y.cos() * sphere.angular_sign);
                let meridian = radial * (-sphere.radius * point.y.sin())
                    + sphere.axis * (sphere.radius * point.y.cos());
                azimuthal * tangent.x + meridian * tangent.y
            }
        }
    }

    pub(crate) const fn as_plane(self) -> Option<Plane> {
        match self {
            Self::Plane(plane) => Some(plane),
            Self::Cylinder(_) | Self::Torus(_) | Self::Cone(_) | Self::Sphere(_) => None,
        }
    }

    pub(crate) fn as_plane_mut(&mut self) -> Option<&mut Plane> {
        match self {
            Self::Plane(plane) => Some(plane),
            Self::Cylinder(_) | Self::Torus(_) | Self::Cone(_) | Self::Sphere(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Face {
    pub(crate) surface: Surface,
    pub(crate) outer_loop: LoopKey,
    /// Ordered, face-owned void boundaries. Each loop is oriented opposite to
    /// `outer_loop` in the face's surface frame and participates in the same
    /// shell incidence graph as the outer boundary.
    pub(crate) inner_loops: Vec<LoopKey>,
    pub(crate) role: FaceRole,
}

impl Face {
    pub(crate) fn loops(&self) -> impl Iterator<Item = LoopKey> + '_ {
        std::iter::once(self.outer_loop).chain(self.inner_loops.iter().copied())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Shell {
    pub(crate) faces: Vec<FaceKey>,
}

#[derive(Clone, Debug)]
pub(crate) struct Solid {
    pub(crate) outer_shell: ShellKey,
    /// Closed internal voids. Each inner shell bounds a cavity and is
    /// oriented away from the material — into the void — so every measure
    /// that sums boundary flux subtracts the cavity without special cases.
    pub(crate) inner_shells: Vec<ShellKey>,
}

impl Solid {
    /// The outer shell followed by every cavity shell.
    pub(crate) fn shells(&self) -> impl Iterator<Item = ShellKey> + '_ {
        std::iter::once(self.outer_shell).chain(self.inner_shells.iter().copied())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Topology {
    pub(crate) vertices: Vec<Record<Vertex>>,
    pub(crate) edges: Vec<Record<Edge>>,
    pub(crate) coedges: Vec<Record<Coedge>>,
    pub(crate) loops: Vec<Record<Loop>>,
    pub(crate) faces: Vec<Record<Face>>,
    pub(crate) shells: Vec<Record<Shell>>,
    pub(crate) solids: Vec<Record<Solid>>,
}

impl Topology {
    pub(crate) fn vertex(&self, key: VertexKey) -> Option<&Record<Vertex>> {
        self.vertices.get(key.0)
    }

    pub(crate) fn edge(&self, key: EdgeKey) -> Option<&Record<Edge>> {
        self.edges.get(key.0)
    }

    pub(crate) fn coedge(&self, key: CoedgeKey) -> Option<&Record<Coedge>> {
        self.coedges.get(key.0)
    }

    pub(crate) fn loop_record(&self, key: LoopKey) -> Option<&Record<Loop>> {
        self.loops.get(key.0)
    }

    pub(crate) fn face(&self, key: FaceKey) -> Option<&Record<Face>> {
        self.faces.get(key.0)
    }

    pub(crate) fn shell(&self, key: ShellKey) -> Option<&Record<Shell>> {
        self.shells.get(key.0)
    }

    pub(crate) fn solid(&self, key: SolidKey) -> Option<&Record<Solid>> {
        self.solids.get(key.0)
    }

    pub(crate) fn oriented_edge_vertices(
        &self,
        coedge: &Coedge,
    ) -> Option<([VertexKey; 2], [Point3; 2])> {
        let edge = &self.edge(coedge.edge)?.value;
        let curve_endpoints = edge.endpoints();
        let (keys, points) = match coedge.orientation {
            Orientation::Forward => (edge.vertices, curve_endpoints),
            Orientation::Reverse => (
                [edge.vertices[1], edge.vertices[0]],
                [curve_endpoints[1], curve_endpoints[0]],
            ),
        };
        Some((keys, points))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TopologyCounts {
    pub vertices: usize,
    pub edges: usize,
    pub coedges: usize,
    pub loops: usize,
    pub faces: usize,
    pub shells: usize,
    pub solids: usize,
}

impl From<&Topology> for TopologyCounts {
    fn from(topology: &Topology) -> Self {
        Self {
            vertices: topology.vertices.len(),
            edges: topology.edges.len(),
            coedges: topology.coedges.len(),
            loops: topology.loops.len(),
            faces: topology.faces.len(),
            shells: topology.shells.len(),
            solids: topology.solids.len(),
        }
    }
}

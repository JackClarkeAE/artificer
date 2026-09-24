//! Recognition: the first half of the button (ADR 0057 §2.2).
//!
//! A body is turned when the kernel can read its `(r, z)` section, milled
//! when every face is a plane or a cylinder along one machine axis and
//! nothing faces downward above the bottom, mill-turn when a turned body
//! carries radial holes or flats, and unsupported otherwise, with the faces
//! that decided it named.

use std::collections::BTreeMap;

use artificer_kernel::{FaceBoundaryCurve2, FaceDescription, FaceGeometry, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, EntityKind, EntityRef, PlanarCurve2, PlanarLoop2, PlanarRegion2, Point2, Point3,
    Vector3,
};

use crate::geom;
use crate::space::{self, Frame};

/// The radial stock allowance a bar gets over the part, in millimetres.
pub const DEFAULT_RADIAL_ALLOWANCE: f64 = 2.0;
/// The facing allowance a bar gets beyond the front face, in millimetres.
pub const DEFAULT_FACING_ALLOWANCE: f64 = 1.0;
/// The stock a milled part gets on each side, in millimetres.
pub const DEFAULT_SIDE_ALLOWANCE: f64 = 2.0;
/// The stock a milled part gets on top, in millimetres.
pub const DEFAULT_TOP_ALLOWANCE: f64 = 1.0;
/// Bar left behind the part-off cut: the parting blade and a remnant.
pub const PART_OFF_MARGIN: f64 = 5.0;

/// One face that decided a classification, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceIssue {
    pub face: u64,
    pub surface: String,
    pub reason: String,
}

/// The stock and the frame a part is machined in.
#[derive(Clone, Debug, PartialEq)]
pub enum Setup {
    Turned(TurnedSetup),
    Milled(MilledSetup),
    MillTurn(MillTurnSetup),
    Unsupported { faces: Vec<FaceIssue> },
}

impl Setup {
    /// The classification as one word, for the card and the tests.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Turned(_) => "Turned",
            Self::Milled(_) => "Milled",
            Self::MillTurn(_) => "MillTurn",
            Self::Unsupported { .. } => "Unsupported",
        }
    }
}

/// The lathe's frame: the work origin sits on the axis at the front face,
/// `direction` runs from the chuck towards the tailstock, and `radial` is the
/// half-plane the section is drawn in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurnAxis {
    pub origin: Point3,
    pub direction: Vector3,
    pub radial: Vector3,
}

impl TurnAxis {
    /// A section point `(r, z)` at azimuth zero, in the world.
    #[must_use]
    pub fn to_world(&self, r: f64, z: f64) -> Point3 {
        space::offset(
            self.origin,
            space::add(
                space::scale(self.direction, z),
                space::scale(self.radial, r),
            ),
        )
    }

    /// A section point `(r, z)` turned to `azimuth`, in the world.
    #[must_use]
    pub fn to_world_at(&self, r: f64, z: f64, azimuth: f64) -> Point3 {
        let tangent = space::cross(self.direction, self.radial);
        let radial = space::add(
            space::scale(self.radial, azimuth.cos()),
            space::scale(tangent, azimuth.sin()),
        );
        space::offset(
            self.origin,
            space::add(space::scale(self.direction, z), space::scale(radial, r)),
        )
    }
}

/// A round bar: its radius, and where it ends in machine `z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarStock {
    pub radius: f64,
    /// The bar's front face, ahead of the part's (positive).
    pub front: f64,
    /// Where the bar is gripped: behind the part-off cut (negative).
    pub back: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurnAllowances {
    pub radial: f64,
    pub facing: f64,
}

/// A turned part, ready for the lathe.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnedSetup {
    pub axis: TurnAxis,
    /// The part's section in machine `(r, z)`: a closed counter-clockwise
    /// loop, `z = 0` at the front face and negative towards the chuck. It
    /// runs along the axis explicitly where the part touches it.
    pub section: PlanarLoop2,
    /// Whether the section is a tube's: clear of the axis, bored through.
    pub through_bore: bool,
    pub length: f64,
    pub max_radius: f64,
    pub stock: BarStock,
    pub allowances: TurnAllowances,
    /// The section's front was the kernel section's low end.
    pub flipped: bool,
    pub faces: Vec<u64>,
    pub part_volume: f64,
}

/// A box of stock around a milled part, in machine coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxStock {
    pub min: Point3,
    pub max: Point3,
}

impl BoxStock {
    #[must_use]
    pub fn volume(&self) -> f64 {
        (self.max.x - self.min.x) * (self.max.y - self.min.y) * (self.max.z - self.min.z)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MillAllowances {
    pub side: f64,
    pub top: f64,
}

/// Where the mill's G-code zero sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkOrigin {
    /// The stock's top-left-front corner: minimum `x`, minimum `y`, top `z`.
    StockCorner,
    /// The middle of the stock top.
    StockCentre,
}

impl WorkOrigin {
    #[must_use]
    pub fn point(self, stock: &BoxStock) -> Point3 {
        match self {
            Self::StockCorner => Point3::new(stock.min.x, stock.min.y, stock.max.z),
            Self::StockCentre => Point3::new(
                (stock.min.x + stock.max.x) / 2.0,
                (stock.min.y + stock.max.y) / 2.0,
                stock.max.z,
            ),
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::StockCorner => "stock corner",
            Self::StockCentre => "stock centre",
        }
    }
}

/// One height a 2.5D part exposes from above, with what is flat there.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub height: f64,
    /// The flat regions at this height, disjoint, in machine `(x, y)`.
    pub regions: Vec<PlanarRegion2>,
    pub faces: Vec<u64>,
}

/// A 2.5D milled part, ready for the mill.
#[derive(Clone, Debug, PartialEq)]
pub struct MilledSetup {
    /// World to machine: `w` is the spindle axis, pointing up out of the stock.
    pub frame: Frame,
    /// The world axis the spindle runs along, as the card names it.
    pub axis_label: &'static str,
    /// Highest first.
    pub levels: Vec<Level>,
    pub top: f64,
    pub bottom: f64,
    pub stock: BoxStock,
    pub work_origin: WorkOrigin,
    pub allowances: MillAllowances,
    pub faces: Vec<u64>,
    pub part_volume: f64,
}

impl MilledSetup {
    /// The G-code zero in machine coordinates.
    #[must_use]
    pub fn origin(&self) -> Point3 {
        self.work_origin.point(&self.stock)
    }

    /// The part's outline seen from above: the union of every level.
    pub fn footprint(&self) -> Result<Vec<PlanarRegion2>, String> {
        let regions = self
            .levels
            .iter()
            .flat_map(|level| level.regions.iter().cloned())
            .collect::<Vec<_>>();
        geom::merge_touching(&regions)
    }

    /// The part's section just above `height`: the union of every level
    /// higher than it.
    pub fn section_above(&self, height: f64) -> Result<Vec<PlanarRegion2>, String> {
        let regions = self
            .levels
            .iter()
            .filter(|level| level.height > height + 1.0e-9)
            .flat_map(|level| level.regions.iter().cloned())
            .collect::<Vec<_>>();
        geom::merge_touching(&regions)
    }
}

/// A turned body with features a lathe cannot make alone.
#[derive(Clone, Debug, PartialEq)]
pub struct MillTurnSetup {
    pub axis: Vector3,
    /// The faces a mill would have to cut after turning: radial holes, flats.
    pub milled_faces: Vec<FaceIssue>,
    pub faces: Vec<u64>,
}

const CANDIDATE_AXES: [(&str, Vector3, Vector3, Vector3); 6] = [
    (
        "+Z",
        space::vector(1.0, 0.0, 0.0),
        space::vector(0.0, 1.0, 0.0),
        space::vector(0.0, 0.0, 1.0),
    ),
    (
        "-Z",
        space::vector(1.0, 0.0, 0.0),
        space::vector(0.0, -1.0, 0.0),
        space::vector(0.0, 0.0, -1.0),
    ),
    (
        "+X",
        space::vector(0.0, 1.0, 0.0),
        space::vector(0.0, 0.0, 1.0),
        space::vector(1.0, 0.0, 0.0),
    ),
    (
        "-X",
        space::vector(0.0, 0.0, 1.0),
        space::vector(0.0, 1.0, 0.0),
        space::vector(-1.0, 0.0, 0.0),
    ),
    (
        "+Y",
        space::vector(0.0, 0.0, 1.0),
        space::vector(1.0, 0.0, 0.0),
        space::vector(0.0, 1.0, 0.0),
    ),
    (
        "-Y",
        space::vector(1.0, 0.0, 0.0),
        space::vector(0.0, 0.0, 1.0),
        space::vector(0.0, -1.0, 0.0),
    ),
];

/// Decides how a body is machined.
#[must_use]
pub fn recognise(snapshot: &Snapshot) -> Setup {
    recognise_with(
        snapshot,
        TurnAllowances {
            radial: DEFAULT_RADIAL_ALLOWANCE,
            facing: DEFAULT_FACING_ALLOWANCE,
        },
        MillAllowances {
            side: DEFAULT_SIDE_ALLOWANCE,
            top: DEFAULT_TOP_ALLOWANCE,
        },
        WorkOrigin::StockCorner,
    )
}

/// [`recognise`] with the allowances and the mill's work origin chosen.
#[must_use]
pub fn recognise_with(
    snapshot: &Snapshot,
    turn_allowances: TurnAllowances,
    mill_allowances: MillAllowances,
    work_origin: WorkOrigin,
) -> Setup {
    let faces = NativeKernel::faces(snapshot);
    if faces.is_empty() {
        return Setup::Unsupported { faces: Vec::new() };
    }
    let descriptions = NativeKernel::describe_faces(snapshot);
    let face_ids = faces.iter().map(|face| face.entity.0).collect::<Vec<_>>();
    let undescribed = faces
        .iter()
        .filter(|face| !descriptions.contains_key(&face.entity.0))
        .map(|face| FaceIssue {
            face: face.entity.0,
            surface: "unknown".to_owned(),
            reason: "its carrier could not be evaluated".to_owned(),
        })
        .collect::<Vec<_>>();
    if !undescribed.is_empty() {
        return Setup::Unsupported { faces: undescribed };
    }
    if let Some(turned) = turned_setup(snapshot, turn_allowances, &face_ids) {
        return Setup::Turned(turned);
    }
    match milled_setup(
        snapshot,
        &descriptions,
        mill_allowances,
        work_origin,
        &face_ids,
    ) {
        Ok(milled) => Setup::Milled(milled),
        Err(milled_issues) => {
            if let Some(mill_turn) = mill_turn_setup(&descriptions, &face_ids) {
                return Setup::MillTurn(mill_turn);
            }
            Setup::Unsupported {
                faces: milled_issues,
            }
        }
    }
}

fn turned_setup(
    snapshot: &Snapshot,
    allowances: TurnAllowances,
    faces: &[u64],
) -> Option<TurnedSetup> {
    let section = NativeKernel::turned_section(snapshot)?;
    if section.sweep < std::f64::consts::TAU - 1.0e-9 {
        // A partial revolve is not a body a lathe makes.
        return None;
    }
    let curves = section.curves.clone();
    if curves.is_empty() {
        return None;
    }
    let mut z_min = f64::INFINITY;
    let mut z_max = f64::NEG_INFINITY;
    let mut r_max: f64 = 0.0;
    let open_loop = PlanarLoop2 {
        curves: curves.clone(),
    };
    if let Some((min, max)) = geom::bounds(&open_loop) {
        z_min = z_min.min(min.y);
        z_max = z_max.max(max.y);
        r_max = r_max.max(max.x);
    }
    let first = geom::curve_start(&curves[0]);
    let last = geom::curve_end(curves.last()?);
    // Where the part meets the axis: between the chain's two ends.
    let (axis_low, axis_high) = if section.closed {
        (f64::NAN, f64::NAN)
    } else {
        (first.y.min(last.y), first.y.max(last.y))
    };
    let bore_at_high = !section.closed && axis_high < z_max - 1.0e-9;
    let bore_at_low = !section.closed && axis_low > z_min + 1.0e-9;
    // The end with the bore faces the tailstock; otherwise the smaller end
    // does, and a tie keeps the section's own high end in front.
    let rim = |z: f64| -> f64 {
        let mut radius: f64 = 0.0;
        for curve in &curves {
            for p in [geom::curve_start(curve), geom::curve_end(curve)] {
                if (p.y - z).abs() <= 1.0e-9 {
                    radius = radius.max(p.x);
                }
            }
        }
        radius
    };
    let front_is_high = if bore_at_high != bore_at_low {
        bore_at_high
    } else {
        rim(z_max) <= rim(z_min) + 1.0e-9
    };
    let z_front = if front_is_high { z_max } else { z_min };
    let map = |p: Point2| -> Point2 {
        if front_is_high {
            Point2::new(p.x, p.y - z_front)
        } else {
            Point2::new(p.x, z_front - p.y)
        }
    };
    let mut machine_curves = curves
        .iter()
        .map(|curve| map_curve(curve, &map, !front_is_high))
        .collect::<Vec<_>>();
    if !section.closed {
        let tail = geom::curve_end(machine_curves.last()?);
        let head = geom::curve_start(&machine_curves[0]);
        if !geom::same_point(tail, head) {
            machine_curves.push(PlanarCurve2::Line {
                start: tail,
                end: head,
            });
        }
    }
    let section_loop = geom::counter_clockwise(&PlanarLoop2 {
        curves: machine_curves,
    });
    let length = z_max - z_min;
    let direction = if front_is_high {
        section.axis
    } else {
        space::scale(section.axis, -1.0)
    };
    let origin = space::offset(section.center, space::scale(section.axis, z_front));
    Some(TurnedSetup {
        axis: TurnAxis {
            origin,
            direction,
            radial: section.radial,
        },
        section: section_loop,
        through_bore: section.closed,
        length,
        max_radius: r_max,
        stock: BarStock {
            radius: r_max + allowances.radial,
            front: allowances.facing,
            back: -(length + PART_OFF_MARGIN),
        },
        allowances,
        flipped: !front_is_high,
        faces: faces.to_vec(),
        part_volume: snapshot.measures().volume,
    })
}

/// A curve through a planar map that may mirror.
fn map_curve(curve: &PlanarCurve2, map: &dyn Fn(Point2) -> Point2, mirrored: bool) -> PlanarCurve2 {
    let flip = |direction: &ArcDirection| {
        if mirrored {
            match direction {
                ArcDirection::CounterClockwise => ArcDirection::Clockwise,
                ArcDirection::Clockwise => ArcDirection::CounterClockwise,
            }
        } else {
            *direction
        }
    };
    match curve {
        PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
            start: map(*start),
            end: map(*end),
        },
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => PlanarCurve2::CircularArc {
            center: map(*center),
            start: map(*start),
            end: map(*end),
            direction: flip(direction),
        },
        PlanarCurve2::Circle {
            center,
            radius,
            direction,
        } => PlanarCurve2::Circle {
            center: map(*center),
            radius: *radius,
            direction: flip(direction),
        },
        PlanarCurve2::Bspline {
            degree,
            control_points,
            knots,
            weights,
        } => PlanarCurve2::Bspline {
            degree: *degree,
            control_points: control_points.iter().map(|p| map(*p)).collect(),
            knots: knots.clone(),
            weights: weights.clone(),
        },
    }
}

fn surface_word(description: &FaceDescription) -> String {
    description.geometry.surface_kind().to_owned()
}

/// How one face sits relative to a candidate spindle axis.
enum FaceStanding {
    Up,
    Down,
    Wall,
    Incompatible(String),
}

fn standing(description: &FaceDescription, w: Vector3) -> FaceStanding {
    match description.geometry {
        FaceGeometry::Plane { .. } => match space::parallel_sign(description.normal, w) {
            1 => FaceStanding::Up,
            -1 => FaceStanding::Down,
            _ if space::perpendicular(description.normal, w) => FaceStanding::Wall,
            _ => FaceStanding::Incompatible("a plane tilted to the spindle axis".to_owned()),
        },
        FaceGeometry::Cylinder { axis, .. } => {
            if space::parallel_sign(axis, w) != 0 {
                FaceStanding::Wall
            } else {
                FaceStanding::Incompatible("a cylinder not along the spindle axis".to_owned())
            }
        }
        FaceGeometry::Cone { .. } => FaceStanding::Incompatible("a cone".to_owned()),
        FaceGeometry::Sphere { .. } => FaceStanding::Incompatible("a sphere".to_owned()),
        FaceGeometry::Torus { .. } => FaceStanding::Incompatible("a torus".to_owned()),
        FaceGeometry::Ruled { .. } => FaceStanding::Incompatible("a ruled surface".to_owned()),
        FaceGeometry::Bspline { .. } => FaceStanding::Incompatible("a B-spline surface".to_owned()),
    }
}

fn milled_setup(
    snapshot: &Snapshot,
    descriptions: &BTreeMap<u64, FaceDescription>,
    allowances: MillAllowances,
    work_origin: WorkOrigin,
    faces: &[u64],
) -> Result<MilledSetup, Vec<FaceIssue>> {
    let bounds = snapshot.measures().bounds.ok_or_else(Vec::new)?;
    let mut best: Option<(usize, &'static str, Frame, Vec<FaceIssue>)> = None;
    for (label, u, v, w) in CANDIDATE_AXES {
        let frame = Frame {
            origin: Point3::new(0.0, 0.0, 0.0),
            u,
            v,
            w,
        };
        let mut issues = Vec::new();
        let mut up = 0_usize;
        let bottom = [frame.to_local(bounds.min).z, frame.to_local(bounds.max).z]
            .into_iter()
            .fold(f64::INFINITY, f64::min);
        for (id, description) in descriptions {
            match standing(description, w) {
                FaceStanding::Up => up += 1,
                FaceStanding::Wall => {}
                FaceStanding::Down => {
                    let height = frame.to_local(description.centre).z;
                    if (height - bottom).abs() > 1.0e-9 * scale_of(bounds) {
                        issues.push(FaceIssue {
                            face: *id,
                            surface: surface_word(description),
                            reason: "faces downward above the bottom: an undercut a mill cannot reach from above".to_owned(),
                        });
                    }
                }
                FaceStanding::Incompatible(reason) => issues.push(FaceIssue {
                    face: *id,
                    surface: surface_word(description),
                    reason,
                }),
            }
        }
        let better = match &best {
            None => true,
            Some((best_up, _, _, best_issues)) => {
                (issues.len(), usize::MAX - up) < (best_issues.len(), usize::MAX - best_up)
            }
        };
        if better {
            best = Some((up, label, frame, issues));
        }
    }
    let (_, axis_label, frame, issues) = best.ok_or_else(Vec::new)?;
    if !issues.is_empty() {
        return Err(issues);
    }

    // Every up-facing face is a level; faces at one height share a level.
    let scale = scale_of(bounds);
    let mut levels: Vec<Level> = Vec::new();
    for (id, description) in descriptions {
        if !matches!(standing(description, frame.w), FaceStanding::Up) {
            continue;
        }
        let face_ref = EntityRef {
            snapshot: snapshot.id(),
            kind: EntityKind::Face,
            entity: artificer_protocol::EntityId(*id),
        };
        let support = NativeKernel::planar_face_support(snapshot, face_ref).map_err(|error| {
            vec![FaceIssue {
                face: *id,
                surface: surface_word(description),
                reason: format!("its boundary could not be read: {}", error.message),
            }]
        })?;
        let height = frame.to_local(description.centre).z;
        let to_machine = |q: Point2| -> Point2 {
            let world = space::offset(
                support.frame.origin,
                space::add(
                    space::scale(support.frame.u, q.x),
                    space::scale(support.frame.v, q.y),
                ),
            );
            let local = frame.to_local(world);
            Point2::new(local.x, local.y)
        };
        // Whether the face frame keeps its handedness seen from above.
        let mirrored = space::dot(space::cross(support.frame.u, support.frame.v), frame.w) < 0.0;
        let outer = loop_from_curves(&support.boundary_curves, &to_machine, mirrored).map_err(
            |reason| {
                vec![FaceIssue {
                    face: *id,
                    surface: surface_word(description),
                    reason,
                }]
            },
        )?;
        let holes = support
            .inner_boundary_curves
            .iter()
            .map(|curves| loop_from_curves(curves, &to_machine, mirrored))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|reason| {
                vec![FaceIssue {
                    face: *id,
                    surface: surface_word(description),
                    reason,
                }]
            })?;
        let region = geom::oriented_region(&PlanarRegion2 { outer, holes });
        match levels
            .iter_mut()
            .find(|level| (level.height - height).abs() <= 1.0e-9 * scale)
        {
            Some(level) => {
                level.regions.push(region);
                level.faces.push(*id);
            }
            None => levels.push(Level {
                height,
                regions: vec![region],
                faces: vec![*id],
            }),
        }
    }
    if levels.is_empty() {
        return Err(vec![]);
    }
    levels.sort_by(|a, b| b.height.total_cmp(&a.height));

    let corners = [
        bounds.min,
        bounds.max,
        Point3::new(bounds.min.x, bounds.min.y, bounds.max.z),
        Point3::new(bounds.min.x, bounds.max.y, bounds.min.z),
        Point3::new(bounds.max.x, bounds.min.y, bounds.min.z),
        Point3::new(bounds.min.x, bounds.max.y, bounds.max.z),
        Point3::new(bounds.max.x, bounds.min.y, bounds.max.z),
        Point3::new(bounds.max.x, bounds.max.y, bounds.min.z),
    ]
    .map(|corner| frame.to_local(corner));
    let mut min = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut max = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for corner in corners {
        min = Point3::new(
            min.x.min(corner.x),
            min.y.min(corner.y),
            min.z.min(corner.z),
        );
        max = Point3::new(
            max.x.max(corner.x),
            max.y.max(corner.y),
            max.z.max(corner.z),
        );
    }
    let stock = BoxStock {
        min: Point3::new(min.x - allowances.side, min.y - allowances.side, min.z),
        max: Point3::new(
            max.x + allowances.side,
            max.y + allowances.side,
            max.z + allowances.top,
        ),
    };
    Ok(MilledSetup {
        frame,
        axis_label,
        levels,
        top: max.z,
        bottom: min.z,
        stock,
        work_origin,
        allowances,
        faces: faces.to_vec(),
        part_volume: snapshot.measures().volume,
    })
}

fn scale_of(bounds: artificer_protocol::Aabb3) -> f64 {
    space::length(space::sub(bounds.max, bounds.min)).max(1.0)
}

/// A face boundary as a loop in machine `(x, y)`.
fn loop_from_curves(
    curves: &[FaceBoundaryCurve2],
    to_machine: &dyn Fn(Point2) -> Point2,
    mirrored: bool,
) -> Result<PlanarLoop2, String> {
    let mut out = Vec::with_capacity(curves.len());
    for curve in curves {
        match *curve {
            FaceBoundaryCurve2::Segment { endpoints } => out.push(PlanarCurve2::Line {
                start: to_machine(endpoints[0]),
                end: to_machine(endpoints[1]),
            }),
            FaceBoundaryCurve2::Arc {
                center,
                u,
                v,
                radius,
                start,
                end,
            } => {
                let determinant = u[0].mul_add(v[1], -(u[1] * v[0]));
                let counter_clockwise = (determinant > 0.0) == (end > start);
                let counter_clockwise = counter_clockwise != mirrored;
                let direction = if counter_clockwise {
                    ArcDirection::CounterClockwise
                } else {
                    ArcDirection::Clockwise
                };
                let machine_center = to_machine(center);
                if (end - start).abs() >= std::f64::consts::TAU - 1.0e-9 {
                    out.push(PlanarCurve2::Circle {
                        center: machine_center,
                        radius,
                        direction,
                    });
                } else {
                    let start_point = to_machine(curve.evaluate(start));
                    let end_point = to_machine(curve.evaluate(end));
                    out.push(PlanarCurve2::CircularArc {
                        center: machine_center,
                        start: start_point,
                        end: end_point,
                        direction,
                    });
                }
            }
        }
    }
    if out.is_empty() {
        return Err("the face has no boundary".to_owned());
    }
    Ok(PlanarLoop2 { curves: out })
}

fn mill_turn_setup(
    descriptions: &BTreeMap<u64, FaceDescription>,
    faces: &[u64],
) -> Option<MillTurnSetup> {
    // The turning axis is the one the most curved area turns about.
    let mut candidates: Vec<(Vector3, Point3, f64)> = Vec::new();
    for description in descriptions.values() {
        let (axis, origin) = match description.geometry {
            FaceGeometry::Cylinder { axis, origin, .. } => (axis, origin),
            FaceGeometry::Cone { axis, apex, .. } => (axis, apex),
            FaceGeometry::Torus { axis, origin, .. } => (axis, origin),
            _ => continue,
        };
        match candidates.iter_mut().find(|(other_axis, other_origin, _)| {
            space::parallel_sign(*other_axis, axis) != 0
                && space::length(space::cross(space::sub(origin, *other_origin), axis)) <= 1.0e-9
        }) {
            Some(entry) => entry.2 += description.area,
            None => candidates.push((axis, origin, description.area)),
        }
    }
    let (axis, origin, _) = candidates.into_iter().max_by(|a, b| a.2.total_cmp(&b.2))?;
    let mut milled = Vec::new();
    for (id, description) in descriptions {
        let coaxial = match description.geometry {
            FaceGeometry::Plane { .. } => space::parallel_sign(description.normal, axis) != 0,
            FaceGeometry::Cylinder {
                axis: face_axis,
                origin: face_origin,
                ..
            }
            | FaceGeometry::Torus {
                axis: face_axis,
                origin: face_origin,
                ..
            } => {
                space::parallel_sign(face_axis, axis) != 0
                    && space::length(space::cross(space::sub(face_origin, origin), axis)) <= 1.0e-9
            }
            FaceGeometry::Cone {
                axis: face_axis,
                apex,
                ..
            } => {
                space::parallel_sign(face_axis, axis) != 0
                    && space::length(space::cross(space::sub(apex, origin), axis)) <= 1.0e-9
            }
            FaceGeometry::Sphere { center, .. } => {
                space::length(space::cross(space::sub(center, origin), axis)) <= 1.0e-9
            }
            FaceGeometry::Ruled { .. } | FaceGeometry::Bspline { .. } => return None,
        };
        if coaxial {
            continue;
        }
        let reason = match description.geometry {
            FaceGeometry::Plane { .. } if space::perpendicular(description.normal, axis) => {
                "a flat along the axis, milled after turning".to_owned()
            }
            FaceGeometry::Cylinder {
                axis: face_axis, ..
            } if space::perpendicular(face_axis, axis) => {
                "a radial hole, drilled after turning".to_owned()
            }
            _ => return None,
        };
        milled.push(FaceIssue {
            face: *id,
            surface: surface_word(description),
            reason,
        });
    }
    if milled.is_empty() {
        return None;
    }
    Some(MillTurnSetup {
        axis,
        milled_faces: milled,
        faces: faces.to_vec(),
    })
}

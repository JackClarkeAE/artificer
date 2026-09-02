//! Shell: a prismatic body hollowed to one uniform wall.
//!
//! A shell open at a cap is a pocket: the cap's boundary offset inward by
//! the wall, mitred at sharp corners, cut to within one wall of the far
//! cap. A shell open at both caps is the same pocket cut through. A closed
//! shell is the body less a core built from the same offset profile, one
//! wall in from every face, joined by the Boolean ladder. Every piece is
//! machinery the kernel already certifies: the mitred loop offset the rim
//! blends use, the exact face cut, the prism constructor and the Boolean
//! engine. A hole through the cap grows by the wall so material stays
//! around it.
//!
//! The domain is the prism: the open cap and the face opposite it are
//! parallel planes, and every other face is generated along their normal,
//! a plane containing it or a cylinder about it. A box is a prism along
//! each of its three axes, so it opens on any face; an extrusion opens on
//! its caps.

use artificer_protocol::{
    ArcDirection, EntityKind, EntityRef, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy, SnapshotId,
    Vector3 as ProtocolVector3,
};

use artificer_protocol::{PlanarAxis2, RevolveAngle};

use crate::analytic_extrusion::{Segment, topology_loop_segments};
use crate::loop_offset::{LoopOffsetError, ReflexPolicy, mitred_inward_offset};
use crate::section_revolve::extract_rz_section;
use crate::topology::{Plane, Point2, Point3, Surface, Topology, Vector3};

/// Why a shell was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellError {
    /// The wall is not a positive length, or leaves no material: an open
    /// shell needs a floor at least one feature size thick, a closed one
    /// a core at least one feature size across.
    WallInvalid,
    /// The body is not a prism about the open cap, or its cap boundary
    /// carries a curve the offset cannot express.
    DomainUnsupported,
    /// An open face does not belong to this snapshot, or is not a face.
    OpenFaceInvalid,
    /// More than two open faces, or two that are not opposite caps.
    OpenFacesUnsupported,
    /// The offset boundary crosses itself: a neck of the cap is thinner
    /// than two walls.
    SelfIntersects,
    /// A sharp reflex corner of the cap involves an arc, which the mitred
    /// offset cannot round.
    ReflexCorner,
    /// The body carries a blend or a dome across the wall. Its inner
    /// surface would be a torus or a sphere with the material on the far
    /// side of the tube, which this release's carriers cannot express.
    BlendUnsupported,
}

impl ShellError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::WallInvalid => "SHELL_WALL_INVALID",
            Self::DomainUnsupported => "SHELL_DOMAIN_UNSUPPORTED",
            Self::OpenFaceInvalid => "SHELL_OPEN_FACE_INVALID",
            Self::OpenFacesUnsupported => "SHELL_OPEN_FACES_UNSUPPORTED",
            Self::SelfIntersects => "SHELL_SELF_INTERSECTS",
            Self::ReflexCorner => "SHELL_REFLEX_CORNER",
            Self::BlendUnsupported => "SHELL_BLEND_UNSUPPORTED",
        }
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::WallInvalid => {
                "The shell wall must be a positive length that leaves a floor, or a core, at least one minimum feature size thick."
            }
            Self::DomainUnsupported => {
                "Shell needs a prism about the open face — that face and the one opposite it parallel planes, every other face a plane or cylinder along their normal — or a coaxial solid of revolution whose section closes through its axis or on itself."
            }
            Self::OpenFaceInvalid => "Every open face must be a face of the supplied snapshot.",
            Self::OpenFacesUnsupported => {
                "A shell opens on one face, on two opposite faces, or on none; on a revolved body every open face must be a cap square to the axis."
            }
            Self::SelfIntersects => {
                "The wall is thicker than half the narrowest neck of the open face, so the offset boundary crosses itself."
            }
            Self::ReflexCorner => {
                "A sharp inside corner of the open face meets an arc; the mitred offset cannot round it."
            }
            Self::BlendUnsupported => {
                "A revolved body's blend or dome cannot be shelled in this release: the wall's inner surface is the offset of a torus or a sphere, whose material would lie on the far side of the tube, and no carrier here expresses that. Shell first, then blend."
            }
        }
    }
}

/// What the shell resolves to: a cut on the open cap, or a core to take
/// away.
#[derive(Clone, Debug)]
pub(crate) enum ShellPlan {
    /// A pocket, blind to within one wall of the far cap or through it.
    Pocket {
        target_face: EntityRef,
        frame: PlanarFrame3,
        profile: PlanarProfile2,
        distance: f64,
    },
    /// The core of a closed shell, built one wall in from every face.
    Hollow {
        frame: PlanarFrame3,
        profile: PlanarProfile2,
        distance: f64,
    },
    /// The core of a revolved body, as the section offset one wall inward
    /// and turned about the same axis. An open cap's run is carried past
    /// the body so the difference opens it rather than meeting it head on.
    Revolved {
        frame: PlanarFrame3,
        profile: PlanarProfile2,
        axis: PlanarAxis2,
        angle: RevolveAngle,
        open: bool,
    },
}

/// The body read as a prism about one planar cap.
struct CapPrism {
    far: usize,
    plane: Plane,
    normal: Vector3,
    height: f64,
    outer: Vec<Segment>,
    holes: Vec<Vec<Segment>>,
}

/// Plans the shell of the single solid in `topology`.
pub(crate) fn plan_shell(
    snapshot: SnapshotId,
    topology: &Topology,
    open: &[EntityRef],
    wall: f64,
    precision: PrecisionPolicy,
) -> Result<ShellPlan, ShellError> {
    let minimum = precision
        .min_feature_size
        .max(precision.modeling_resolution);
    if !wall.is_finite() || wall <= minimum {
        return Err(ShellError::WallInvalid);
    }
    if topology.solids.len() != 1 {
        return Err(ShellError::DomainUnsupported);
    }
    let mut open_indices = Vec::with_capacity(open.len());
    for face in open {
        if face.snapshot != snapshot || face.kind != EntityKind::Face {
            return Err(ShellError::OpenFaceInvalid);
        }
        let index = topology
            .faces
            .iter()
            .position(|record| record.id.get() == face.entity.0)
            .ok_or(ShellError::OpenFaceInvalid)?;
        if !open_indices.contains(&index) {
            open_indices.push(index);
        }
    }

    match plan_prismatic_shell(topology, &open_indices, open, wall, minimum, precision) {
        Ok(plan) => Ok(plan),
        // A body the prism reading cannot own may still be a solid of
        // revolution. Its section carries the whole boundary, blends
        // included, so the offset happens there instead and the core comes
        // back as a revolve.
        Err(ShellError::DomainUnsupported) => {
            plan_revolved_shell(topology, &open_indices, wall, precision)
        }
        Err(other) => Err(other),
    }
}

/// The prism reading: the open cap's outline offset inward, as a pocket or
/// as a core.
fn plan_prismatic_shell(
    topology: &Topology,
    open_indices: &[usize],
    open: &[EntityRef],
    wall: f64,
    minimum: f64,
    precision: PrecisionPolicy,
) -> Result<ShellPlan, ShellError> {
    match open_indices {
        [] => {
            // Any planar face the body is a prism about serves as the cap
            // of a closed shell; the core is the same whichever is chosen.
            let cap = (0..topology.faces.len())
                .find_map(|index| cap_prism(topology, index, precision).ok())
                .ok_or(ShellError::DomainUnsupported)?;
            let distance = cap.height - 2.0 * wall;
            if distance <= minimum {
                return Err(ShellError::WallInvalid);
            }
            let profile = offset_profile(&cap, wall, precision)?;
            // The core starts one wall above the far cap and rises along
            // the cap normal to one wall below the cap.
            let origin = cap.plane.origin + cap.normal * -(cap.height - wall);
            Ok(ShellPlan::Hollow {
                frame: frame_at(origin, cap.plane),
                profile,
                distance,
            })
        }
        [index] => {
            let cap = cap_prism(topology, *index, precision)?;
            let distance = cap.height - wall;
            if distance <= minimum {
                return Err(ShellError::WallInvalid);
            }
            let profile = offset_profile(&cap, wall, precision)?;
            Ok(ShellPlan::Pocket {
                target_face: open[0],
                frame: frame_at(cap.plane.origin, cap.plane),
                profile,
                distance,
            })
        }
        [index, other] => {
            let cap = cap_prism(topology, *index, precision)?;
            if cap.far != *other {
                return Err(ShellError::OpenFacesUnsupported);
            }
            let profile = offset_profile(&cap, wall, precision)?;
            Ok(ShellPlan::Pocket {
                target_face: open[0],
                frame: frame_at(cap.plane.origin, cap.plane),
                profile,
                distance: cap.height,
            })
        }
        _ => Err(ShellError::OpenFacesUnsupported),
    }
}

/// Reads the body as a prism about the planar face `cap`.
fn cap_prism(
    topology: &Topology,
    cap: usize,
    precision: PrecisionPolicy,
) -> Result<CapPrism, ShellError> {
    let face = &topology.faces[cap].value;
    let plane = match face.surface {
        Surface::Plane(plane) => plane,
        _ => return Err(ShellError::DomainUnsupported),
    };
    let scale = topology
        .vertices
        .iter()
        .map(|vertex| {
            let point = vertex.value.point;
            point.x.abs().max(point.y.abs()).max(point.z.abs())
        })
        .fold(1.0_f64, f64::max);
    let agreement = precision.linear_agreement.max(1.0e-9) * scale;
    let length = plane.normal.length();
    if !length.is_finite() || length <= f64::EPSILON {
        return Err(ShellError::DomainUnsupported);
    }
    // The loops are read in the plane's own parameters, so those must be
    // isotropic for arcs to stay circles.
    if (plane.u.length() - 1.0).abs() > agreement || (plane.v.length() - 1.0).abs() > agreement {
        return Err(ShellError::DomainUnsupported);
    }
    let normal = plane.normal / length;

    let mut far = None;
    for (index, other) in topology.faces.iter().enumerate() {
        if index == cap {
            continue;
        }
        let along = match other.value.surface {
            Surface::Plane(other_plane) => {
                let other_length = other_plane.normal.length();
                if !other_length.is_finite() || other_length <= f64::EPSILON {
                    return Err(ShellError::DomainUnsupported);
                }
                let other_normal = other_plane.normal / other_length;
                if (other_normal + normal).length() <= agreement {
                    if far.replace(index).is_some() {
                        return Err(ShellError::DomainUnsupported);
                    }
                    continue;
                }
                other_normal.dot(normal).abs() <= agreement
            }
            Surface::Cylinder(cylinder) => cylinder.axis.cross(normal).length() <= agreement,
            Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => false,
        };
        if !along {
            return Err(ShellError::DomainUnsupported);
        }
    }
    let far = far.ok_or(ShellError::DomainUnsupported)?;
    let far_plane = match topology.faces[far].value.surface {
        Surface::Plane(plane) => plane,
        _ => return Err(ShellError::DomainUnsupported),
    };
    let height = (plane.origin - far_plane.origin).dot(normal);
    if !height.is_finite() || height <= precision.min_feature_size {
        return Err(ShellError::DomainUnsupported);
    }
    let outer =
        topology_loop_segments(topology, face.outer_loop).ok_or(ShellError::DomainUnsupported)?;
    let holes = face
        .inner_loops
        .iter()
        .map(|loop_key| topology_loop_segments(topology, *loop_key))
        .collect::<Option<Vec<_>>>()
        .ok_or(ShellError::DomainUnsupported)?;
    Ok(CapPrism {
        far,
        plane,
        normal,
        height,
        outer,
        holes,
    })
}

/// The cap's boundary one wall in: the outer loop shrunk, every hole
/// grown. Both run with the material on their left, so one inward offset
/// serves both.
fn offset_profile(
    cap: &CapPrism,
    wall: f64,
    precision: PrecisionPolicy,
) -> Result<PlanarProfile2, ShellError> {
    let offset = |source: &[Segment]| -> Result<PlanarLoop2, ShellError> {
        let spine = mitred_inward_offset(source, wall, ReflexPolicy::MitreLines, precision)
            .map_err(|reason| match reason {
                LoopOffsetError::RadiusTooLarge => ShellError::WallInvalid,
                LoopOffsetError::SelfIntersects => ShellError::SelfIntersects,
                LoopOffsetError::ReflexSharpCorner => ShellError::ReflexCorner,
                LoopOffsetError::Degenerate => ShellError::DomainUnsupported,
            })?;
        // Neighbouring offsets meet at one point each; an arc's own end
        // may differ from its neighbour's start in the last bits, and the
        // profile reader wants the chain exact, so every curve runs from
        // the point its segment starts at to the point the next one does.
        let count = spine.segments.len();
        let starts = spine
            .segments
            .iter()
            .map(|segment| segment.start())
            .collect::<Vec<_>>();
        let curves = spine
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| curve(*segment, starts[index], starts[(index + 1) % count]))
            .collect::<Option<Vec<_>>>()
            .ok_or(ShellError::DomainUnsupported)?;
        Ok(PlanarLoop2 { curves })
    };
    let outer = offset(&cap.outer)?;
    let holes = cap
        .holes
        .iter()
        .map(|hole| offset(hole))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PlanarProfile2 {
        regions: vec![PlanarRegion2 { outer, holes }],
    })
}

/// One offset segment as a profile curve. Section traces never reach a
/// prism cap, so an ellipse or a harmonic is outside the domain.
fn curve(segment: Segment, start: Point2, end: Point2) -> Option<PlanarCurve2> {
    let point = |point: Point2| ProtocolPoint2::new(point.x, point.y);
    match segment {
        Segment::Line { .. } => Some(PlanarCurve2::Line {
            start: point(start),
            end: point(end),
        }),
        Segment::Arc {
            center,
            radius,
            sweep,
            ..
        } => {
            let direction = if sweep >= 0.0 {
                ArcDirection::CounterClockwise
            } else {
                ArcDirection::Clockwise
            };
            if (sweep.abs() - std::f64::consts::TAU).abs() <= 1.0e-9 {
                Some(PlanarCurve2::Circle {
                    center: point(center),
                    radius,
                    direction,
                })
            } else {
                Some(PlanarCurve2::CircularArc {
                    center: point(center),
                    start: point(start),
                    end: point(end),
                    direction,
                })
            }
        }
        Segment::Ellipse { .. } | Segment::Harmonic { .. } => None,
    }
}

/// The cap plane's frame, moved to `origin` along its normal.
fn frame_at(origin: Point3, plane: Plane) -> PlanarFrame3 {
    PlanarFrame3 {
        origin: ProtocolPoint3::new(origin.x, origin.y, origin.z),
        u: ProtocolVector3::new(plane.u.x, plane.u.y, plane.u.z),
        v: ProtocolVector3::new(plane.v.x, plane.v.y, plane.v.z),
    }
}

// ---------------------------------------------------------------------------
// Solids of revolution
// ---------------------------------------------------------------------------

/// One run of a section loop, and what the offset must do with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    /// Boundary the wall follows: offset inward by the wall.
    Wall,
    /// A cap the shell opens. Displacing it two walls outward leaves it one
    /// wall beyond the body after the offset, so the difference opens the
    /// cap instead of meeting it head on.
    Open,
    /// The axis the section closes through. Displacing it one wall outward
    /// puts it back on `r = 0` after the offset, where the core's own
    /// closure belongs.
    Axis,
}

/// The revolved reading: the section offset one wall inward and turned
/// about the same axis, as the core to take away.
///
/// The section carries the body's whole boundary — cones, spheres and the
/// tori of a rim blend included — so offsetting it is the true offset of
/// the solid, and the wall is one wall thick measured square to the
/// surface everywhere.
fn plan_revolved_shell(
    topology: &Topology,
    open_indices: &[usize],
    wall: f64,
    precision: PrecisionPolicy,
) -> Result<ShellPlan, ShellError> {
    let section = extract_rz_section(topology).map_err(|_| ShellError::DomainUnsupported)?;
    let axis = section.axis();
    let center = section.center();
    let scale = topology
        .vertices
        .iter()
        .map(|vertex| {
            let point = vertex.value.point;
            point.x.abs().max(point.y.abs()).max(point.z.abs())
        })
        .fold(1.0_f64, f64::max);
    let agreement = precision.linear_agreement.max(1.0e-9) * scale;

    // Every open face must be a cap square to the axis; its height along
    // the axis is what locates the section run it stands for.
    let mut heights = Vec::with_capacity(open_indices.len());
    for index in open_indices {
        let Surface::Plane(plane) = topology.faces[*index].value.surface else {
            return Err(ShellError::OpenFacesUnsupported);
        };
        let length = plane.normal.length();
        if !length.is_finite()
            || length <= f64::EPSILON
            || plane.normal.cross(axis).length() > agreement * length
        {
            return Err(ShellError::OpenFacesUnsupported);
        }
        heights.push((plane.origin - center).dot(axis));
    }

    // A section arc is a blend band or a dome. Offsetting it inward is
    // exact, but the surface that comes back bounds the wall from the
    // inside, with the material on the far side of the tube from where a
    // torus or a sphere in this kernel can carry it. Refusing here says so
    // once, rather than leaving the validator to reject the frame later.
    if section
        .segments()
        .iter()
        .any(|segment| !matches!(segment, Segment::Line { .. }))
    {
        return Err(ShellError::BlendUnsupported);
    }

    let mut runs: Vec<(Segment, Run)> = Vec::with_capacity(section.segments().len() + 1);
    let mut opened = 0;
    for segment in section.segments() {
        let cap_height = match segment {
            Segment::Line { start, end } if (start.y - end.y).abs() <= agreement => Some(start.y),
            _ => None,
        };
        let open = cap_height.is_some_and(|height| {
            heights
                .iter()
                .any(|wanted| (wanted - height).abs() <= agreement)
        });
        if open {
            opened += 1;
        }
        runs.push((*segment, if open { Run::Open } else { Run::Wall }));
    }
    if opened < heights.len() {
        return Err(ShellError::OpenFacesUnsupported);
    }

    // A chain that does not close on itself closes through the axis, and
    // that closure is a run of the loop like any other.
    if !section.is_closed() {
        let first = runs.first().ok_or(ShellError::DomainUnsupported)?.0.start();
        let last = runs.last().ok_or(ShellError::DomainUnsupported)?.0.end();
        if first.x.abs() > agreement || last.x.abs() > agreement {
            return Err(ShellError::DomainUnsupported);
        }
        runs.push((
            Segment::Line {
                start: last,
                end: first,
            },
            Run::Axis,
        ));
    }

    // The offset reads a counter-clockwise loop with the material on its
    // left. Which way the extractor chained the section depends on the
    // body, so the loop is turned the right way round here.
    let area: f64 = runs
        .iter()
        .map(|(segment, _)| segment.signed_area_contribution())
        .sum();
    if !area.is_finite() || area.abs() <= agreement * agreement {
        return Err(ShellError::DomainUnsupported);
    }
    if area < 0.0 {
        runs.reverse();
        for run in &mut runs {
            run.0 = reversed(run.0).ok_or(ShellError::DomainUnsupported)?;
        }
    }

    let mut displaced = Vec::with_capacity(runs.len());
    for (segment, kind) in &runs {
        displaced.push(match kind {
            Run::Wall => *segment,
            Run::Axis => displaced_outward(*segment, wall).ok_or(ShellError::DomainUnsupported)?,
            Run::Open => {
                displaced_outward(*segment, 2.0 * wall).ok_or(ShellError::DomainUnsupported)?
            }
        });
    }

    let spine = mitred_inward_offset(&displaced, wall, ReflexPolicy::MitreLines, precision)
        .map_err(|reason| match reason {
            LoopOffsetError::RadiusTooLarge => ShellError::WallInvalid,
            LoopOffsetError::SelfIntersects => ShellError::SelfIntersects,
            LoopOffsetError::ReflexSharpCorner => ShellError::ReflexCorner,
            LoopOffsetError::Degenerate => ShellError::DomainUnsupported,
        })?;

    let count = spine.segments.len();
    let starts: Vec<Point2> = spine
        .segments
        .iter()
        .map(|segment| segment.start())
        .collect();
    if starts.iter().any(|point| point.x < -agreement) {
        // The wall swallowed the axis: there is no core left to take away.
        return Err(ShellError::WallInvalid);
    }
    let curves = spine
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| curve(*segment, starts[index], starts[(index + 1) % count]))
        .collect::<Option<Vec<_>>>()
        .ok_or(ShellError::DomainUnsupported)?;

    let radial_u = section.radial_u();
    Ok(ShellPlan::Revolved {
        frame: PlanarFrame3 {
            origin: ProtocolPoint3::new(center.x, center.y, center.z),
            u: ProtocolVector3::new(radial_u.x, radial_u.y, radial_u.z),
            v: ProtocolVector3::new(axis.x, axis.y, axis.z),
        },
        profile: PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 { curves },
                holes: Vec::new(),
            }],
        },
        axis: PlanarAxis2::new(ProtocolPoint2::new(0.0, 0.0), ProtocolPoint2::new(0.0, 1.0)),
        angle: RevolveAngle::FullTurn,
        open: !open_indices.is_empty(),
    })
}

/// A straight run moved `distance` away from the material, which for a
/// counter-clockwise loop lies to the left of travel. Only a straight run
/// can move without changing shape, so an arc has no displacement.
fn displaced_outward(segment: Segment, distance: f64) -> Option<Segment> {
    let Segment::Line { start, end } = segment else {
        return None;
    };
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length = dx.hypot(dy);
    if !length.is_finite() || length <= f64::EPSILON {
        return None;
    }
    let offset = Point2::new(dy / length * distance, -dx / length * distance);
    Some(Segment::Line {
        start: Point2::new(start.x + offset.x, start.y + offset.y),
        end: Point2::new(end.x + offset.x, end.y + offset.y),
    })
}

/// One section run travelled the other way.
fn reversed(segment: Segment) -> Option<Segment> {
    match segment {
        Segment::Line { start, end } => Some(Segment::Line {
            start: end,
            end: start,
        }),
        Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep,
        } => Some(Segment::Arc {
            center,
            start: end,
            end: start,
            radius,
            start_angle: start_angle + sweep,
            sweep: -sweep,
        }),
        Segment::Ellipse { .. } | Segment::Harmonic { .. } => None,
    }
}

/// Encloses `core` inside `target` as a void.
///
/// The core is the target's own boundary offset one wall inward, so it lies
/// inside the material by construction: an inward offset that does not
/// self-intersect is contained in the region it came from, and the offset
/// refuses by name when it does. Nothing here has to rediscover that, so
/// no Boolean classification runs. The core's faces are reversed, because
/// a void's boundary faces into the void rather than out of the material
/// it once enclosed, and its shell becomes an inner shell of the target's
/// solid. The ordinary solid validator certifies the result before commit.
pub(crate) fn hollow(target: &Topology, core: &Topology) -> Option<Topology> {
    if target.solids.len() != 1
        || core.solids.len() != 1
        || !core.solids[0].value.inner_shells.is_empty()
        || core.shells.len() != 1
    {
        return None;
    }
    let faces_before = target.faces.len();
    let mut merged = crate::pattern::merge_disjoint(target, core);
    for index in faces_before..merged.faces.len() {
        reverse_face(&mut merged, index)?;
    }
    let void = merged.solids.pop()?.value.outer_shell;
    merged.solids.first_mut()?.value.inner_shells.push(void);
    Some(merged)
}

/// Turns one face inside out: its carrier's parameterisation is flipped and
/// its loops walk the other way, so the face bounds the space it used to
/// exclude.
fn reverse_face(topology: &mut Topology, face_index: usize) -> Option<()> {
    let mirror: fn(Point2) -> Point2 = match &mut topology.faces[face_index].value.surface {
        Surface::Plane(plane) => {
            *plane = Plane::new(plane.origin, plane.v, plane.u);
            |point: Point2| Point2::new(point.y, point.x)
        }
        Surface::Cylinder(cylinder) => {
            cylinder.angular_sign = -cylinder.angular_sign;
            |point: Point2| Point2::new(-point.x, point.y)
        }
        Surface::Cone(cone) => {
            cone.angular_sign = -cone.angular_sign;
            |point: Point2| Point2::new(-point.x, point.y)
        }
        // A torus and a sphere pin their frame to their angular sign, so
        // the outward normal is always the geometric one: the material
        // lies inside the tube, and no edit of the frame moves it out.
        Surface::Torus(_) | Surface::Sphere(_) => return None,
    };
    crate::mirror::reverse_face_loops(topology, face_index, mirror).ok()
}

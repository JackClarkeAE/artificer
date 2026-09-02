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

use crate::analytic_extrusion::{Segment, topology_loop_segments};
use crate::loop_offset::{LoopOffsetError, ReflexPolicy, mitred_inward_offset};
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
        }
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::WallInvalid => {
                "The shell wall must be a positive length that leaves a floor, or a core, at least one minimum feature size thick."
            }
            Self::DomainUnsupported => {
                "Shell needs a prism about the open face: the face and the one opposite it parallel planes, every other face a plane or cylinder along their normal, with a boundary of lines and arcs."
            }
            Self::OpenFaceInvalid => "Every open face must be a face of the supplied snapshot.",
            Self::OpenFacesUnsupported => {
                "A shell opens on one face, on two opposite faces, or on none."
            }
            Self::SelfIntersects => {
                "The wall is thicker than half the narrowest neck of the open face, so the offset boundary crosses itself."
            }
            Self::ReflexCorner => {
                "A sharp inside corner of the open face meets an arc; the mitred offset cannot round it."
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

    match open_indices.as_slice() {
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

//! The coaxial Boolean (ADR 0026 F4): two solids of revolution about one
//! axis meet only in their shared section.
//!
//! Turning is a bijection between a body of revolution and its half-section
//! in `(r, z)`, and it commutes with union, intersection and difference. So
//! a Boolean of two coaxial bodies is the same Boolean of their sections,
//! taken in the plane by the exact line/arc engine and turned again by the
//! revolve builder. Every carrier the section builder makes — plane,
//! cylinder, cone, sphere, torus — comes back exact, which the 3D engines
//! cannot do for the last three.
//!
//! The domain is narrow on purpose. Both bodies must be single solids the
//! section extractor reads, about one axis, both full turns or both the same
//! partial turn from the same azimuth. Anything else is `NotCoaxial`, and
//! the caller carries on down its own ladder.

use std::f64::consts::TAU;

use artificer_protocol::{
    ArcDirection, BooleanOperation, PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3,
    PrecisionPolicy, RevolveAngle, Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::Segment;
use crate::profile_boolean::{ProfileBooleanError, ProfileRegion, profile_boolean_multi};
use crate::revolve::{build_revolve, validate_revolve};
use crate::section_revolve::{RzSection, extract_rz_section};
use crate::topology::{Point2, Topology};

/// Why the coaxial route stood aside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoaxialBooleanError {
    /// Either body is not a solid of revolution the extractor reads, or the
    /// two do not share an axis and a span.
    NotCoaxial,
    /// The sections meet in a way the plane engine refuses (a tangency or a
    /// sliver), or the result does not turn into a valid solid.
    Unsupported,
    /// Nothing is left.
    EmptyResult,
}

/// The Boolean of two coaxial bodies of revolution, rebuilt from the
/// Boolean of their sections.
pub(crate) fn coaxial_boolean(
    target: &Topology,
    tool: &Topology,
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Topology, CoaxialBooleanError> {
    let first = extract_rz_section(target).map_err(|_| CoaxialBooleanError::NotCoaxial)?;
    let second = extract_rz_section(tool).map_err(|_| CoaxialBooleanError::NotCoaxial)?;

    let scale = scale_of(target).max(scale_of(tool));
    let agreement = precision.linear_agreement.max(1.0e-9) * scale;
    let (axis, center) = (first.axis(), first.center());
    let (other_axis, other_center) = (second.axis(), second.center());
    if axis.cross(other_axis).length() > 1.0e-9 {
        return Err(CoaxialBooleanError::NotCoaxial);
    }
    let offset = other_center - center;
    if (offset - axis * offset.dot(axis)).length() > agreement {
        return Err(CoaxialBooleanError::NotCoaxial);
    }
    // The tool's heights, read along the target's axis.
    let shift = offset.dot(axis);
    let sense = if axis.dot(other_axis) > 0.0 {
        1.0
    } else {
        -1.0
    };

    // A full turn with a full turn, or one partial span with the same span:
    // the same sweep, from the same azimuth, about the same way.
    let full = |section: &RzSection| section.sweep() >= TAU - 1.0e-9;
    let angle = match (full(&first), full(&second)) {
        (true, true) => RevolveAngle::FullTurn,
        (false, false)
            if sense > 0.0
                && (first.sweep() - second.sweep()).abs() <= 1.0e-9
                && (first.radial_u() - second.radial_u()).length() <= 1.0e-9 =>
        {
            RevolveAngle::partial(0.0, first.sweep())
        }
        _ => return Err(CoaxialBooleanError::NotCoaxial),
    };

    let regions = profile_boolean_multi(
        &[section_region(&first, 0.0, 1.0)?],
        &[section_region(&second, shift, sense)?],
        operation,
        precision,
    )
    .map_err(|reason| match reason {
        ProfileBooleanError::EmptyResult => CoaxialBooleanError::EmptyResult,
        ProfileBooleanError::Unsupported => CoaxialBooleanError::Unsupported,
    })?;

    let profile = PlanarProfile2 {
        regions: regions
            .iter()
            .map(|region| {
                Ok(PlanarRegion2 {
                    outer: planar_loop(&region.outer)?,
                    holes: region
                        .holes
                        .iter()
                        .map(|hole| planar_loop(hole))
                        .collect::<Result<_, _>>()?,
                })
            })
            .collect::<Result<_, CoaxialBooleanError>>()?,
    };
    let radial_u = first.radial_u();
    let frame = PlanarFrame3 {
        origin: ProtocolPoint3::new(center.x, center.y, center.z),
        u: ProtocolVector3::new(radial_u.x, radial_u.y, radial_u.z),
        v: ProtocolVector3::new(axis.x, axis.y, axis.z),
    };
    let revolved = validate_revolve(
        frame,
        &profile,
        PlanarAxis2::new(ProtocolPoint2::new(0.0, 0.0), ProtocolPoint2::new(0.0, 1.0)),
        angle,
        precision,
    )
    .map_err(|_| CoaxialBooleanError::Unsupported)?;
    Ok(build_revolve(&revolved))
}

/// A section as a closed region in the target's `(r, z)`: the tool's heights
/// moved by `shift` and turned by `sense`, and a chain that ends on the axis
/// closed along it.
fn section_region(
    section: &RzSection,
    shift: f64,
    sense: f64,
) -> Result<ProfileRegion, CoaxialBooleanError> {
    let place = |point: Point2| Point2::new(point.x, shift + sense * point.y);
    let mut outer = section
        .segments()
        .iter()
        .map(|segment| placed(*segment, &place, sense))
        .collect::<Option<Vec<_>>>()
        .ok_or(CoaxialBooleanError::NotCoaxial)?;
    if !section.is_closed() {
        let (Some(first), Some(last)) = (outer.first(), outer.last()) else {
            return Err(CoaxialBooleanError::NotCoaxial);
        };
        outer.push(Segment::Line {
            start: last.end(),
            end: first.start(),
        });
    }
    Ok(ProfileRegion {
        outer: merged_arcs(outer),
        holes: Vec::new(),
    })
}

/// The loop with each run of arcs on one circle made one arc. A torus is
/// built in an inner and an outer half, split where its section crosses
/// the major radius; that split is the builder's, not the profile's, and a
/// cut through it would meet the other section at a vertex, which the plane
/// engine refuses.
fn merged_arcs(segments: Vec<Segment>) -> Vec<Segment> {
    let same_circle = |first: &Segment, second: &Segment| match (*first, *second) {
        (
            Segment::Arc {
                center,
                radius,
                sweep,
                ..
            },
            Segment::Arc {
                center: other_center,
                radius: other_radius,
                sweep: other_sweep,
                ..
            },
        ) => {
            let tolerance = 1.0e-12 * (1.0 + radius.abs());
            (center.x - other_center.x).abs() <= tolerance
                && (center.y - other_center.y).abs() <= tolerance
                && (radius - other_radius).abs() <= tolerance
                && sweep.signum() == other_sweep.signum()
        }
        _ => false,
    };
    let join = |first: Segment, second: Segment| match (first, second) {
        (
            Segment::Arc {
                center,
                start,
                radius,
                start_angle,
                sweep,
                ..
            },
            Segment::Arc {
                end, sweep: more, ..
            },
        ) => Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep: sweep + more,
        },
        _ => first,
    };
    let mut merged: Vec<Segment> = Vec::with_capacity(segments.len());
    for segment in segments {
        match merged.last_mut() {
            Some(last) if same_circle(last, &segment) => *last = join(*last, segment),
            _ => merged.push(segment),
        }
    }
    // The loop closes on itself, so its last run may continue its first.
    while merged.len() > 1 && same_circle(&merged[merged.len() - 1], &merged[0]) {
        let last = merged.pop().expect("more than one");
        merged[0] = join(last, merged[0]);
    }
    merged
}

/// One section run in the target's frame. Turning the heights over mirrors
/// an arc, which then sweeps the other way.
fn placed(segment: Segment, place: &impl Fn(Point2) -> Point2, sense: f64) -> Option<Segment> {
    match segment {
        Segment::Line { start, end } => Some(Segment::Line {
            start: place(start),
            end: place(end),
        }),
        Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep,
        } => Some(Segment::Arc {
            center: place(center),
            start: place(start),
            end: place(end),
            radius,
            start_angle: sense * start_angle,
            sweep: sense * sweep,
        }),
        Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => None,
    }
}

/// A result loop as profile curves, each run from the point its segment
/// starts at to the point the next one does, so the chain is exact.
fn planar_loop(segments: &[Segment]) -> Result<PlanarLoop2, CoaxialBooleanError> {
    let count = segments.len();
    let point = |point: Point2| ProtocolPoint2::new(point.x, point.y);
    let curves = segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let (start, end) = (segment.start(), segments[(index + 1) % count].start());
            match *segment {
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
                    Some(if (sweep.abs() - TAU).abs() <= 1.0e-9 {
                        PlanarCurve2::Circle {
                            center: point(center),
                            radius,
                            direction,
                        }
                    } else {
                        PlanarCurve2::CircularArc {
                            center: point(center),
                            start: point(start),
                            end: point(end),
                            direction,
                        }
                    })
                }
                Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => None,
            }
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(CoaxialBooleanError::Unsupported)?;
    Ok(PlanarLoop2 { curves })
}

fn scale_of(topology: &Topology) -> f64 {
    topology
        .vertices
        .iter()
        .map(|vertex| {
            let point = vertex.value.point;
            point.x.abs().max(point.y.abs()).max(point.z.abs())
        })
        .fold(1.0_f64, f64::max)
}

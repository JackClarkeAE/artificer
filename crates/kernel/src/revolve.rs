//! Full-turn revolves of a certified planar profile (ADR 0026, F3).
//!
//! The kernel already knew how to build every surface a revolve needs: a
//! coaxial solid of revolution is exactly its `(r, z)` section, and
//! [`section_revolve::build_revolved_topology`] turns that section into
//! cylinders, cones, tori, spheres, and planar caps. What was missing was a way
//! for a user to reach it. This module is that mapping, and nothing more: it
//! certifies the profile against the axis, rewrites it into the section
//! half-plane, and hands the chain over unchanged.
//!
//! The section half-plane is reached by one rotation. Choosing the radial
//! direction so that `(radial, axis)` is right-handed in the profile frame
//! makes that rotation orientation-preserving, so a counter-clockwise profile
//! arrives as a counter-clockwise section — the winding the builder already
//! expects, with no case analysis and no chance of an inside-out solid.

use artificer_protocol::{
    MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS, PlanarAxis2,
    PlanarFrame3, PlanarProfile2, PrecisionPolicy, RevolveAngle,
};

use crate::analytic_extrusion::{Segment, normalize_frame, parse_loop, reversed_loop};
use crate::planar_profile::PlanarProfileInputError;
use crate::section_revolve::{RzSection, build_revolved_topology};
use crate::topology::{FaceRole, Point2, Topology, Vector3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RevolveInputError {
    /// The profile itself is not a certified planar region.
    Profile(PlanarProfileInputError),
    /// v1 revolves exactly one region without holes.
    SingleRegionOnly,
    /// The axis endpoints coincide, so there is no axis.
    DegenerateAxis,
    /// Material lies on both sides of the axis; the sweep would self-intersect.
    ProfileCrossesAxis,
    /// A straight segment meets the axis obliquely. It would sweep a cone apex
    /// — a singular point rather than a pole — which stays outside the domain.
    ObliqueAxisContact,
    /// The chain left after dropping axis-collinear segments is not one
    /// contiguous section.
    SectionNotContiguous,
}

impl From<PlanarProfileInputError> for RevolveInputError {
    fn from(reason: PlanarProfileInputError) -> Self {
        Self::Profile(reason)
    }
}

#[derive(Debug)]
pub(crate) struct ValidatedRevolve {
    section: RzSection,
}

#[must_use]
pub(crate) fn build_revolve(revolve: &ValidatedRevolve) -> Topology {
    build_revolved_topology(&revolve.section)
}

/// Certifies a profile and axis, and rewrites the profile as a section chain.
pub(crate) fn validate_revolve(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    axis: PlanarAxis2,
    angle: RevolveAngle,
    precision: PrecisionPolicy,
) -> Result<ValidatedRevolve, RevolveInputError> {
    let RevolveAngle::FullTurn = angle;
    if profile.regions.is_empty() {
        return Err(PlanarProfileInputError::EmptyProfile.into());
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return Err(PlanarProfileInputError::TooManyRegions.into());
    }
    if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
        return Err(PlanarProfileInputError::TooManyLoops.into());
    }
    if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
        return Err(PlanarProfileInputError::TooManyCurves.into());
    }
    // A hole in the profile sweeps a cavity of revolution, which the single
    // section chain cannot express; it needs the coaxial Boolean rung.
    if profile.regions.len() != 1 || !profile.regions[0].holes.is_empty() {
        return Err(RevolveInputError::SingleRegionOnly);
    }
    if !frame.is_finite() || !axis.is_finite() {
        return Err(PlanarProfileInputError::Extrusion(
            crate::extrusion::ExtrusionInputError::NonFinite,
        )
        .into());
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let frame = normalize_frame(frame, precision)?;
    let mut region = parse_loop(
        &profile.regions[0].outer,
        minimum,
        precision.linear_agreement,
    )?;
    if region.signed_area.abs() <= minimum * minimum {
        return Err(PlanarProfileInputError::Extrusion(
            crate::extrusion::ExtrusionInputError::AreaTooSmall,
        )
        .into());
    }
    if region.signed_area < 0.0 {
        region = reversed_loop(region);
    }

    // The axis in the profile's own frame, and the radial direction that makes
    // `(radial, axis)` right-handed there.
    let origin = Point2::new(axis.start.x, axis.start.y);
    let span = Point2::new(axis.end.x - axis.start.x, axis.end.y - axis.start.y);
    let length = span.x.hypot(span.y);
    if !length.is_finite() || length <= minimum {
        return Err(RevolveInputError::DegenerateAxis);
    }
    let mut along = Point2::new(span.x / length, span.y / length);
    let mut radial = Point2::new(along.y, -along.x);

    let radius_of = |point: Point2, radial: Point2| {
        (point.x - origin.x).mul_add(radial.x, (point.y - origin.y) * radial.y)
    };
    let extent = region
        .segments
        .iter()
        .flat_map(|segment| [segment.start(), segment.end()])
        .fold(1.0_f64, |extent, point| {
            extent.max(point.x.abs().max(point.y.abs()))
        });
    let on_axis = precision.linear_agreement.max(1.0e-12) * extent;
    let side = |radial: Point2| {
        region
            .segments
            .iter()
            .flat_map(|segment| [segment.start(), segment.end()])
            .map(|point| radius_of(point, radial))
            .fold((false, false), |(negative, positive), radius| {
                (negative || radius < -on_axis, positive || radius > on_axis)
            })
    };
    match side(radial) {
        (true, true) => return Err(RevolveInputError::ProfileCrossesAxis),
        (true, false) => {
            // The material is on the other side. Reversing the axis reverses
            // the radial direction with it, so the frame stays right-handed
            // and the section still lands in the positive half-plane.
            along = Point2::new(-along.x, -along.y);
            radial = Point2::new(along.y, -along.x);
        }
        _ => {}
    }

    // The section rotation: r along `radial`, z along `along`.
    let to_section = |point: Point2| {
        Point2::new(
            radius_of(point, radial).max(0.0),
            (point.x - origin.x).mul_add(along.x, (point.y - origin.y) * along.y),
        )
    };
    let phase = radial.y.atan2(radial.x);
    let mut chain = Vec::with_capacity(region.segments.len());
    for segment in &region.segments {
        let start = to_section(segment.start());
        let end = to_section(segment.end());
        let section = match *segment {
            Segment::Line { .. } => {
                let start_on_axis = start.x <= on_axis;
                let end_on_axis = end.x <= on_axis;
                if start_on_axis && end_on_axis {
                    // The axis-collinear closure. It sweeps nothing and emits
                    // no face; the builder closes the chain through the axis.
                    continue;
                }
                if (start_on_axis || end_on_axis)
                    && (end.y - start.y).abs() > on_axis
                    && (end.x - start.x).abs() > on_axis
                {
                    return Err(RevolveInputError::ObliqueAxisContact);
                }
                Segment::Line { start, end }
            }
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            } => Segment::Arc {
                center: to_section(center),
                start,
                end,
                radius,
                start_angle: start_angle - phase,
                sweep,
            },
            Segment::Ellipse { .. } | Segment::Harmonic { .. } => {
                unreachable!("revolve profiles carry lines and arcs only")
            }
        };
        chain.push(section);
    }

    let Some(first) = chain.first().copied() else {
        return Err(RevolveInputError::SectionNotContiguous);
    };
    for pair in chain.windows(2) {
        if !meets(pair[0].end(), pair[1].start(), on_axis) {
            return Err(RevolveInputError::SectionNotContiguous);
        }
    }
    let last = chain[chain.len() - 1];
    let closed = meets(last.end(), first.start(), on_axis);
    // A chain that does not close on itself must begin and end on the axis,
    // because the axis is then what closes it.
    let closes_through_axis = first.start().x <= on_axis && last.end().x <= on_axis;
    if !(closed || closes_through_axis) {
        return Err(RevolveInputError::SectionNotContiguous);
    }

    let roles = (0..chain.len())
        .map(|index| FaceRole::ExtrusionSide(u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    let center = frame.point(origin, 0.0);
    let axis_direction = frame.u * along.x + frame.v * along.y;
    let radial_u = frame.u * radial.x + frame.v * radial.y;
    let radial_v = cross(axis_direction, radial_u);
    Ok(ValidatedRevolve {
        section: RzSection::from_parts(
            center,
            axis_direction,
            radial_u,
            radial_v,
            chain,
            roles,
            closed,
        ),
    })
}

fn meets(left: Point2, right: Point2, agreement: f64) -> bool {
    (left.x - right.x).hypot(left.y - right.y) <= agreement
}

fn cross(left: Vector3, right: Vector3) -> Vector3 {
    Vector3::new(
        left.y.mul_add(right.z, -(left.z * right.y)),
        left.z.mul_add(right.x, -(left.x * right.z)),
        left.x.mul_add(right.y, -(left.y * right.x)),
    )
}

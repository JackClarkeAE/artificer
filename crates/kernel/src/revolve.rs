//! Revolves of a certified planar profile (ADR 0026 F3, ADR 0055).
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
//!
//! A partial turn is measured about the axis as the caller gave it. When the
//! profile lies on the other side of that axis, the section frame's axis is
//! the reverse of it, so the requested span of azimuths is mirrored before
//! the frame is turned to where the span begins.

use std::f64::consts::TAU;

use artificer_protocol::{
    MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS, PlanarAxis2,
    PlanarFrame3, PlanarProfile2, PrecisionPolicy, RevolveAngle,
};

use crate::analytic_extrusion::{Segment, normalize_frame, parse_loop, reversed_loop};
use crate::planar_profile::PlanarProfileInputError;
use crate::section_revolve::{RzSection, build_turned_topology};
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
    /// The chain left after dropping axis-collinear segments is not one
    /// contiguous section.
    SectionNotContiguous,
    /// A partial turn's sweep is not strictly between nothing and a full
    /// turn, its start is not a finite angle within a turn, or the sweep or
    /// the gap it leaves is narrower than the minimum feature at the
    /// profile's outermost radius.
    AngleInvalid,
}

impl From<PlanarProfileInputError> for RevolveInputError {
    fn from(reason: PlanarProfileInputError) -> Self {
        Self::Profile(reason)
    }
}

#[derive(Debug)]
pub(crate) struct ValidatedRevolve {
    section: RzSection,
    /// How far the section turns from its frame's azimuth zero: a full turn,
    /// or less.
    sweep: f64,
}

impl ValidatedRevolve {
    /// Whether this revolve stops short of a full turn.
    pub(crate) fn is_partial(&self) -> bool {
        self.sweep < TAU
    }
}

#[must_use]
pub(crate) fn build_revolve(revolve: &ValidatedRevolve) -> Topology {
    build_turned_topology(&revolve.section, revolve.sweep)
}

/// Certifies a profile and axis, and rewrites the profile as a section chain.
pub(crate) fn validate_revolve(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    axis: PlanarAxis2,
    angle: RevolveAngle,
    precision: PrecisionPolicy,
) -> Result<ValidatedRevolve, RevolveInputError> {
    let turn = match angle {
        RevolveAngle::FullTurn => None,
        RevolveAngle::Partial { start, sweep } => {
            if !start.is_finite() || !sweep.is_finite() || start.abs() > TAU {
                return Err(RevolveInputError::AngleInvalid);
            }
            if sweep <= 0.0 || sweep >= TAU {
                return Err(RevolveInputError::AngleInvalid);
            }
            Some((start, sweep))
        }
    };
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
    let reversed_axis = match side(radial) {
        (true, true) => return Err(RevolveInputError::ProfileCrossesAxis),
        (true, false) => {
            // The material is on the other side. Reversing the axis reverses
            // the radial direction with it, so the frame stays right-handed
            // and the section still lands in the positive half-plane.
            along = Point2::new(-along.x, -along.y);
            radial = Point2::new(along.y, -along.x);
            true
        }
        _ => false,
    };

    // The section rotation: r along `radial`, z along `along`. A point within
    // agreement of the axis is on it, exactly, so that it closes a cap or a
    // pole rather than sweeping a vanishing ring.
    let to_section = |point: Point2| {
        let radius = radius_of(point, radial);
        Point2::new(
            if radius <= on_axis { 0.0 } else { radius },
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
                // A slanted line reaching the axis sweeps a cone to its
                // apex, which closes through a pole as a sphere does.
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
            Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => {
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

    // The span, in the section frame's own azimuth. About a reversed axis the
    // requested span runs the other way round, so it is mirrored.
    let (begin, sweep) = match turn {
        None => (0.0, TAU),
        Some((start, sweep)) => {
            let outermost = chain.iter().map(outermost_radius).fold(0.0_f64, f64::max);
            if sweep * outermost < minimum || (TAU - sweep) * outermost < minimum {
                return Err(RevolveInputError::AngleInvalid);
            }
            if reversed_axis {
                (-(start + sweep), sweep)
            } else {
                (start, sweep)
            }
        }
    };

    let roles = (0..chain.len())
        .map(|index| FaceRole::ExtrusionSide(u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    let center = frame.point(origin, 0.0);
    let axis_direction = frame.u * along.x + frame.v * along.y;
    let profile_radial = frame.u * radial.x + frame.v * radial.y;
    let profile_tangent = cross(axis_direction, profile_radial);
    // Turn the frame to where the span begins; a full turn begins at the
    // profile itself.
    let (radial_u, radial_v) = if begin == 0.0 {
        (profile_radial, profile_tangent)
    } else {
        let radial_u = profile_radial * begin.cos() + profile_tangent * begin.sin();
        (radial_u, cross(axis_direction, radial_u))
    };
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
        sweep,
    })
}

/// The farthest a section segment reaches from the axis: its endpoints, or an
/// arc's bulge beyond them.
fn outermost_radius(segment: &Segment) -> f64 {
    let ends = segment.start().x.max(segment.end().x);
    match *segment {
        Segment::Arc { center, radius, .. } => ends.max(center.x + radius),
        _ => ends,
    }
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

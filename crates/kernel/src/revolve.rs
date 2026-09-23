//! Revolves of a certified planar profile (ADR 0026 F3, ADR 0055).
//!
//! The kernel already knew how to build every surface a revolve needs: a
//! coaxial solid of revolution is exactly its `(r, z)` section, and
//! [`section_revolve::build_turned_region`] turns that section into
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

use crate::analytic_extrusion::{
    AnalyticLoop, Segment, angle_on_arc, merge_topologies, normalize_frame, parse_loop,
    reversed_loop, validate_disjoint_loop_regions, validate_hole_nesting,
};
use crate::planar_profile::PlanarProfileInputError;
use crate::section_revolve::{RzSection, build_turned_region};
use crate::topology::{FaceRole, Point2, Topology, Vector3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RevolveInputError {
    /// The profile itself is not a certified planar region.
    Profile(PlanarProfileInputError),
    /// A hole in the profile reaches the axis, so it would not sweep a
    /// cavity or a channel clear of it.
    HoleOnAxis,
    /// The axis endpoints coincide, so there is no axis.
    DegenerateAxis,
    /// Material lies on both sides of the axis; the sweep would self-intersect.
    ProfileCrossesAxis,
    /// The chain left after dropping axis-collinear segments is not one
    /// contiguous section.
    SectionNotContiguous,
    /// The section reaches the axis at a single point — a corner or an
    /// arc's tangency — rather than along a run of it, so the turned solid
    /// would be pinched to a point there.
    PinchedOnAxis,
    /// An arc is centred on the far side of the axis, so it would turn into
    /// the inner lemon of a spindle torus.
    ArcCentreAcrossAxis,
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
    /// Each region's section and the sections of its holes, all in the one
    /// section frame. A region's section runs anticlockwise and a hole's
    /// clockwise, so material is on the left of every chain.
    regions: Vec<(RzSection, Vec<RzSection>)>,
    /// How far the sections turn from their frame's azimuth zero: a full
    /// turn, or less.
    sweep: f64,
}

impl ValidatedRevolve {
    /// Whether this revolve stops short of a full turn.
    pub(crate) fn is_partial(&self) -> bool {
        self.sweep < TAU
    }
}

/// The solid a revolve sweeps: one per region, each hole a cavity inside it
/// for a full turn or a channel through it for less.
#[must_use]
pub(crate) fn build_revolve(revolve: &ValidatedRevolve) -> Topology {
    // One region is built exactly as it always was, entity for entity.
    if let [(outer, holes)] = revolve.regions.as_slice() {
        return build_turned_region(outer, holes, revolve.sweep);
    }
    merge_topologies(
        revolve
            .regions
            .iter()
            .map(|(outer, holes)| build_turned_region(outer, holes, revolve.sweep))
            .collect(),
    )
}

/// Certifies a profile and axis, and rewrites every loop of the profile as a
/// section chain.
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
    let non_finite = || {
        RevolveInputError::from(PlanarProfileInputError::Extrusion(
            crate::extrusion::ExtrusionInputError::NonFinite,
        ))
    };
    if !frame.is_finite()
        || !axis.is_finite()
        || profile
            .regions
            .iter()
            .flat_map(|region| std::iter::once(&region.outer).chain(&region.holes))
            .flat_map(|profile_loop| &profile_loop.curves)
            .any(|curve| !curve.is_finite())
    {
        return Err(non_finite());
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let frame = normalize_frame(frame, precision)?;
    let area_too_small = || {
        RevolveInputError::from(PlanarProfileInputError::Extrusion(
            crate::extrusion::ExtrusionInputError::AreaTooSmall,
        ))
    };
    // Every region as its loops, the outer one first and anticlockwise and
    // its holes clockwise, so that material is on the left of every one. The
    // holes must lie inside the outer loop and apart from each other, and the
    // regions apart from one another, exactly as for an extrusion.
    let mut regions = Vec::<Vec<AnalyticLoop>>::with_capacity(profile.regions.len());
    for region in &profile.regions {
        let mut outer = parse_loop(&region.outer, minimum, precision.linear_agreement)?;
        if outer.signed_area.abs() <= minimum * minimum {
            return Err(area_too_small());
        }
        if outer.signed_area < 0.0 {
            outer = reversed_loop(outer);
        }
        let mut loops = Vec::with_capacity(1 + region.holes.len());
        loops.push(outer);
        for hole in &region.holes {
            let mut hole = parse_loop(hole, minimum, precision.linear_agreement)?;
            if hole.signed_area.abs() <= minimum * minimum {
                return Err(area_too_small());
            }
            if hole.signed_area > 0.0 {
                hole = reversed_loop(hole);
            }
            loops.push(hole);
        }
        validate_hole_nesting(&loops, minimum)?;
        let net_area = loops
            .iter()
            .map(|profile_loop| profile_loop.signed_area)
            .sum::<f64>();
        if !net_area.is_finite() || net_area <= minimum * minimum {
            return Err(area_too_small());
        }
        regions.push(loops);
    }
    validate_disjoint_loop_regions(
        &regions.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        minimum,
    )?;
    let every_segment = || {
        regions
            .iter()
            .flatten()
            .flat_map(|profile_loop| &profile_loop.segments)
    };

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

    // Everything the sweep reaches lies within the profile's farthest reach
    // from the axis's start, about that start's place in the world.
    let reach = every_segment()
        .map(|segment| {
            let ends = distance(segment.start(), origin).max(distance(segment.end(), origin));
            match *segment {
                Segment::Arc { center, radius, .. } => ends.max(distance(center, origin) + radius),
                _ => ends,
            }
        })
        .fold(0.0_f64, f64::max);
    let coordinate_limit = precision.max_abs_coordinate;
    let axis_place = frame.point(origin, 0.0);
    if every_segment()
        .flat_map(|segment| [segment.start(), segment.end()])
        .flat_map(|point| [point.x, point.y])
        .chain([axis.start.x, axis.start.y, axis.end.x, axis.end.y])
        .chain([axis_place.x, axis_place.y, axis_place.z].map(|value| value.abs() + reach))
        .any(|value| !value.is_finite() || value.abs() > coordinate_limit)
    {
        return Err(PlanarProfileInputError::Extrusion(
            crate::extrusion::ExtrusionInputError::CoordinateLimit,
        )
        .into());
    }

    let radius_of = |point: Point2, radial: Point2| {
        (point.x - origin.x).mul_add(radial.x, (point.y - origin.y) * radial.y)
    };
    let extent = every_segment()
        .flat_map(|segment| [segment.start(), segment.end()])
        .fold(1.0_f64, |extent, point| {
            extent.max(point.x.abs().max(point.y.abs()))
        });
    let on_axis = precision.linear_agreement.max(1.0e-12) * extent;
    // Which sides of the axis the profile reaches: every endpoint, and every
    // arc's bulge towards or away from the axis, which a circle whose
    // endpoints all lie on one side can carry across it.
    let side = |radial: Point2| {
        let toward = radial.y.atan2(radial.x);
        every_segment()
            .flat_map(|segment| {
                let bulges = match *segment {
                    Segment::Arc {
                        center,
                        radius,
                        start_angle,
                        sweep,
                        ..
                    } => [toward, toward + std::f64::consts::PI].map(|angle| {
                        angle_on_arc(angle, start_angle, sweep, 0.0).then(|| {
                            Point2::new(
                                radius.mul_add(angle.cos(), center.x),
                                radius.mul_add(angle.sin(), center.y),
                            )
                        })
                    }),
                    _ => [None, None],
                };
                [Some(segment.start()), Some(segment.end())]
                    .into_iter()
                    .chain(bulges)
                    .flatten()
            })
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
    // An arc's centre may lie across the axis; it snaps onto the axis, for a
    // sphere, only when it is on it.
    let centre_to_section = |point: Point2| {
        let radius = radius_of(point, radial);
        Point2::new(
            if radius.abs() <= on_axis { 0.0 } else { radius },
            (point.x - origin.x).mul_add(along.x, (point.y - origin.y) * along.y),
        )
    };
    let phase = radial.y.atan2(radial.x);
    let collinear_with_axis = |segment: &Segment| {
        matches!(segment, Segment::Line { .. })
            && radius_of(segment.start(), radial) <= on_axis
            && radius_of(segment.end(), radial) <= on_axis
    };
    // One loop as a section chain, and whether it closes on itself.
    let chain_of =
        |profile_loop: &AnalyticLoop| -> Result<(Vec<Segment>, bool), RevolveInputError> {
            // Begin just after a run along the axis, wherever the loop was
            // started, so that the run is the chain's ends and not its middle.
            let count = profile_loop.segments.len();
            let begin = (0..count)
                .find(|&index| {
                    collinear_with_axis(&profile_loop.segments[index])
                        && !collinear_with_axis(&profile_loop.segments[(index + 1) % count])
                })
                .map_or(0, |index| (index + 1) % count);
            let mut chain = Vec::with_capacity(count);
            for offset in 0..count {
                let segment = &profile_loop.segments[(begin + offset) % count];
                if collinear_with_axis(segment) {
                    // The axis-collinear closure. It sweeps nothing and emits
                    // no face; the builder closes the chain through the axis.
                    continue;
                }
                let start = to_section(segment.start());
                let end = to_section(segment.end());
                let section = match *segment {
                    // A slanted line reaching the axis sweeps a cone to its
                    // apex, which closes through a pole as a sphere does.
                    Segment::Line { .. } => Segment::Line { start, end },
                    Segment::Arc {
                        center,
                        radius,
                        start_angle,
                        sweep,
                        ..
                    } => {
                        let center = centre_to_section(center);
                        // Turned, an arc centred across the axis is the
                        // inner lemon of a spindle torus, a carrier the
                        // kernel does not certify.
                        if center.x < 0.0 {
                            return Err(RevolveInputError::ArcCentreAcrossAxis);
                        }
                        Segment::Arc {
                            center,
                            start,
                            end,
                            radius,
                            start_angle: start_angle - phase,
                            sweep,
                        }
                    }
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
            // A chain that does not close on itself must begin and end on the
            // axis, because the axis is then what closes it.
            let closes_through_axis = first.start().x <= on_axis && last.end().x <= on_axis;
            if !(closed || closes_through_axis) {
                return Err(RevolveInputError::SectionNotContiguous);
            }
            // Anywhere else the section reaches the axis — a corner, where
            // the chain closes on itself, or an arc's tangency — the turned
            // solid is pinched to a point there, which no manifold is.
            let corner_on_axis = chain.windows(2).any(|pair| pair[0].end().x <= on_axis)
                || (closed && first.start().x <= on_axis);
            if corner_on_axis
                || chain
                    .iter()
                    .any(|segment| nearest_radius_within(segment) <= on_axis)
            {
                return Err(RevolveInputError::PinchedOnAxis);
            }
            Ok((chain, closed))
        };
    let mut chains = Vec::with_capacity(regions.len());
    for loops in &regions {
        let outer = chain_of(&loops[0])?;
        let mut hole_chains = Vec::with_capacity(loops.len() - 1);
        for hole in &loops[1..] {
            // A hole sweeps a cavity or a channel only while it stays clear
            // of the axis all the way round.
            let (chain, closed) = chain_of(hole).map_err(|reason| match reason {
                RevolveInputError::PinchedOnAxis | RevolveInputError::SectionNotContiguous => {
                    RevolveInputError::HoleOnAxis
                }
                other => other,
            })?;
            if !closed
                || chain
                    .iter()
                    .any(|segment| segment.start().x <= on_axis || segment.end().x <= on_axis)
            {
                return Err(RevolveInputError::HoleOnAxis);
            }
            hole_chains.push(chain);
        }
        chains.push((outer, hole_chains));
    }

    // The span, in the section frame's own azimuth. About a reversed axis the
    // requested span runs the other way round, so it is mirrored.
    let (begin, sweep) = match turn {
        None => (0.0, TAU),
        Some((start, sweep)) => {
            let outermost = chains
                .iter()
                .flat_map(|((outer, _), holes)| std::iter::once(outer).chain(holes))
                .flatten()
                .map(outermost_radius)
                .fold(0.0_f64, f64::max);
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
    // Every segment of every loop names its own side face.
    let mut next_role = 0_u32;
    let mut section = |chain: Vec<Segment>, closed: bool| {
        let roles = chain
            .iter()
            .map(|_| {
                let role = FaceRole::ExtrusionSide(next_role);
                next_role = next_role.saturating_add(1);
                role
            })
            .collect();
        RzSection::from_parts(
            center,
            axis_direction,
            radial_u,
            radial_v,
            chain,
            roles,
            closed,
        )
    };
    let regions = chains
        .into_iter()
        .map(|((outer, closed), holes)| {
            let outer = section(outer, closed);
            let holes = holes.into_iter().map(|hole| section(hole, true)).collect();
            (outer, holes)
        })
        .collect();
    Ok(ValidatedRevolve { regions, sweep })
}

/// The nearest a section segment comes to the axis between its ends: an arc's
/// bulge towards the axis, or nothing for a line, which is nearest at an end.
fn nearest_radius_within(segment: &Segment) -> f64 {
    match *segment {
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let toward = start_angle + (std::f64::consts::PI - start_angle).rem_euclid(TAU);
            let progress = if sweep > 0.0 {
                (toward - start_angle) / sweep
            } else {
                (start_angle - toward).rem_euclid(TAU) / -sweep
            };
            if progress > 0.0 && progress < 1.0 {
                center.x - radius
            } else {
                f64::INFINITY
            }
        }
        _ => f64::INFINITY,
    }
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

fn distance(left: Point2, right: Point2) -> f64 {
    (left.x - right.x).hypot(left.y - right.y)
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

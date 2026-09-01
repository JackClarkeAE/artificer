//! Native exact extrusion of planar profiles containing circular geometry.
//!
//! Curves and surfaces remain analytic in the authoritative B-rep. Complete
//! circles are deliberately split into two semicircle edges so every loop has
//! explicit vertices and every cylindrical wall has two seam generators.

use artificer_protocol::{
    ArcDirection, MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PrecisionPolicy,
};

use crate::extrusion::ExtrusionInputError;
use crate::planar_profile::PlanarProfileInputError;
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Cylinder, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole,
    Loop, LoopKey, Orientation, ParameterRange, Plane, Point2, Point3, Record, Shell, ShellKey,
    Solid, Surface, Topology, Vector2, Vector3, Vertex, VertexKey,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub(crate) origin: Point3,
    pub(crate) u: Vector3,
    pub(crate) v: Vector3,
    pub(crate) normal: Vector3,
}

impl Frame {
    pub(crate) fn point(self, point: Point2, height: f64) -> Point3 {
        self.origin + self.u * point.x + self.v * point.y + self.normal * height
    }

    pub(crate) fn center(self, point: Point2, height: f64) -> Point3 {
        self.point(point, height)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Segment {
    Line {
        start: Point2,
        end: Point2,
    },
    Arc {
        center: Point2,
        start: Point2,
        end: Point2,
        radius: f64,
        start_angle: f64,
        sweep: f64,
    },
}

impl Segment {
    pub(crate) const fn start(self) -> Point2 {
        match self {
            Self::Line { start, .. } | Self::Arc { start, .. } => start,
        }
    }

    pub(crate) const fn end(self) -> Point2 {
        match self {
            Self::Line { end, .. } | Self::Arc { end, .. } => end,
        }
    }

    /// Whether two consecutive exact profile pieces sweep the same logical
    /// side carrier. Full circles are represented internally as two
    /// semicircular arcs; preserving that analytic implementation detail as
    /// two user-facing cylinder faces would manufacture a seam.
    pub(crate) fn shares_side_carrier(self, other: Self) -> bool {
        match (self, other) {
            (
                Self::Arc {
                    center: first_center,
                    radius: first_radius,
                    sweep: first_sweep,
                    ..
                },
                Self::Arc {
                    center: second_center,
                    radius: second_radius,
                    sweep: second_sweep,
                    ..
                },
            ) => {
                let scale = first_radius
                    .abs()
                    .max(second_radius.abs())
                    .max(first_center.x.abs())
                    .max(first_center.y.abs())
                    .max(second_center.x.abs())
                    .max(second_center.y.abs())
                    .max(1.0);
                (first_center.x - second_center.x).hypot(first_center.y - second_center.y)
                    <= scale * 1.0e-9
                    && (first_radius - second_radius).abs() <= scale * 1.0e-9
                    && first_sweep.signum() == second_sweep.signum()
            }
            _ => false,
        }
    }

    pub(crate) fn signed_area_contribution(self) -> f64 {
        let start = self.start();
        let end = self.end();
        match self {
            Self::Line { .. } => 0.5 * (start.x * end.y - start.y * end.x),
            Self::Arc {
                center,
                radius,
                sweep,
                ..
            } => {
                0.5 * (center.x * (end.y - start.y) - center.y * (end.x - start.x)
                    + radius * radius * sweep)
            }
        }
    }

    fn length(self) -> f64 {
        match self {
            Self::Line { start, end } => (end.x - start.x).hypot(end.y - start.y),
            Self::Arc { radius, sweep, .. } => radius * sweep.abs(),
        }
    }

    pub(crate) fn translated(self, anchor: Point2) -> Self {
        let shift = |point: Point2| Point2::new(point.x - anchor.x, point.y - anchor.y);
        match self {
            Self::Line { start, end } => Self::Line {
                start: shift(start),
                end: shift(end),
            },
            Self::Arc {
                center,
                start,
                end,
                radius,
                start_angle,
                sweep,
            } => Self::Arc {
                center: shift(center),
                start: shift(start),
                end: shift(end),
                radius,
                start_angle,
                sweep,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AnalyticLoop {
    pub(crate) segments: Vec<Segment>,
    pub(crate) signed_area: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedAnalyticExtrusion {
    pub(crate) regions: Vec<ValidatedAnalyticRegionExtrusion>,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedAnalyticRegionExtrusion {
    pub(crate) frame: Frame,
    pub(crate) loops: Vec<AnalyticLoop>,
    pub(crate) distance: f64,
}

pub(crate) fn validate_analytic_profile_extrusion(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<ValidatedAnalyticExtrusion, PlanarProfileInputError> {
    if profile.regions.is_empty() {
        return Err(PlanarProfileInputError::EmptyProfile);
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return Err(PlanarProfileInputError::TooManyRegions);
    }
    if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
        return Err(PlanarProfileInputError::TooManyLoops);
    }
    if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
        return Err(PlanarProfileInputError::TooManyCurves);
    }
    if !frame.is_finite()
        || !distance.is_finite()
        || profile
            .regions
            .iter()
            .flat_map(|region| std::iter::once(&region.outer).chain(&region.holes))
            .flat_map(|profile_loop| &profile_loop.curves)
            .any(|curve| !curve.is_finite())
    {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::NonFinite,
        ));
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if distance <= 0.0 {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::NonPositiveDistance,
        ));
    }
    if distance <= minimum {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::FeatureTooSmall,
        ));
    }
    let frame = normalize_frame(frame, precision)?;
    let mut regions = Vec::with_capacity(profile.regions.len());
    for region in &profile.regions {
        let mut outer = parse_loop(&region.outer, minimum, precision.linear_agreement)?;
        if outer.signed_area.abs() <= minimum * minimum {
            return Err(PlanarProfileInputError::Extrusion(
                ExtrusionInputError::AreaTooSmall,
            ));
        }
        if outer.signed_area < 0.0 {
            outer = reversed_loop(outer);
        }
        let mut loops = vec![outer];
        for hole in &region.holes {
            let mut parsed = parse_loop(hole, minimum, precision.linear_agreement)?;
            if parsed.signed_area.abs() <= minimum * minimum {
                return Err(PlanarProfileInputError::Extrusion(
                    ExtrusionInputError::AreaTooSmall,
                ));
            }
            if parsed.signed_area > 0.0 {
                parsed = reversed_loop(parsed);
            }
            loops.push(parsed);
        }
        validate_hole_nesting(&loops, minimum)?;
        let net_area = loops
            .iter()
            .map(|profile_loop| profile_loop.signed_area)
            .sum::<f64>();
        if !net_area.is_finite() || net_area <= minimum * minimum {
            return Err(PlanarProfileInputError::Extrusion(
                ExtrusionInputError::AreaTooSmall,
            ));
        }
        regions.push(ValidatedAnalyticRegionExtrusion {
            frame,
            loops,
            distance,
        });
    }
    validate_disjoint_regions(&regions, minimum)?;
    let coordinate_limit = precision.max_abs_coordinate;
    let mut coordinates = regions
        .iter()
        .flat_map(|region| &region.loops)
        .flat_map(|profile_loop| &profile_loop.segments)
        .flat_map(|segment| {
            let start = segment.start();
            let end = segment.end();
            [start.x, start.y, end.x, end.y]
        })
        .chain([frame.origin.x, frame.origin.y, frame.origin.z, distance]);
    if coordinates.any(|value| !value.is_finite() || value.abs() > coordinate_limit) {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::CoordinateLimit,
        ));
    }
    for region in &regions {
        for segment in region
            .loops
            .iter()
            .flat_map(|profile_loop| &profile_loop.segments)
        {
            if let Segment::Arc { center, radius, .. } = *segment {
                let world_center = frame.center(center, 0.0);
                let world_top_center = frame.center(center, distance);
                let carrier_envelope = [
                    center.x.abs() + radius,
                    center.y.abs() + radius,
                    world_center.x.abs() + radius * frame.u.x.hypot(frame.v.x),
                    world_center.y.abs() + radius * frame.u.y.hypot(frame.v.y),
                    world_center.z.abs() + radius * frame.u.z.hypot(frame.v.z),
                    world_top_center.x.abs() + radius * frame.u.x.hypot(frame.v.x),
                    world_top_center.y.abs() + radius * frame.u.y.hypot(frame.v.y),
                    world_top_center.z.abs() + radius * frame.u.z.hypot(frame.v.z),
                ];
                if carrier_envelope
                    .into_iter()
                    .any(|value| !value.is_finite() || value > coordinate_limit)
                {
                    return Err(PlanarProfileInputError::Extrusion(
                        ExtrusionInputError::CoordinateLimit,
                    ));
                }
            }
            let bottom_start = frame.point(segment.start(), 0.0);
            let bottom_end = frame.point(segment.end(), 0.0);
            let top_start = frame.point(segment.start(), distance);
            let top_end = frame.point(segment.end(), distance);
            if [bottom_start, bottom_end, top_start, top_end]
                .into_iter()
                .flat_map(|point| [point.x, point.y, point.z])
                .any(|value| !value.is_finite() || value.abs() > coordinate_limit)
            {
                return Err(PlanarProfileInputError::Extrusion(
                    ExtrusionInputError::CoordinateLimit,
                ));
            }
            if bottom_start.distance(bottom_end) <= precision.linear_agreement
                && !matches!(segment, Segment::Arc { sweep, .. } if sweep.abs() >= std::f64::consts::PI)
            {
                return Err(PlanarProfileInputError::Extrusion(
                    ExtrusionInputError::PrecisionUnrepresentable,
                ));
            }
        }
    }

    Ok(ValidatedAnalyticExtrusion { regions })
}

pub(crate) fn reversed_loop(profile_loop: AnalyticLoop) -> AnalyticLoop {
    let segments = profile_loop
        .segments
        .into_iter()
        .rev()
        .map(|segment| match segment {
            Segment::Line { start, end } => Segment::Line {
                start: end,
                end: start,
            },
            Segment::Arc {
                center,
                start,
                end,
                radius,
                start_angle,
                sweep,
            } => Segment::Arc {
                center,
                start: end,
                end: start,
                radius,
                start_angle: start_angle + sweep,
                sweep: -sweep,
            },
        })
        .collect();
    AnalyticLoop {
        segments,
        signed_area: -profile_loop.signed_area,
    }
}

pub(crate) fn normalize_frame(
    frame: PlanarFrame3,
    precision: PrecisionPolicy,
) -> Result<Frame, PlanarProfileInputError> {
    let raw_u = Vector3::new(frame.u.x, frame.u.y, frame.u.z);
    let raw_v = Vector3::new(frame.v.x, frame.v.y, frame.v.z);
    let Some(u) = robust_unit(raw_u) else {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::DegenerateFrame,
        ));
    };
    let Some(raw_v) = robust_unit(raw_v) else {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::DegenerateFrame,
        ));
    };
    let cross = u.cross(raw_v);
    let angular_floor = precision
        .angular_agreement_radians
        .clamp(64.0 * f64::EPSILON, 1.0);
    if !cross.length().is_finite() || cross.length() <= angular_floor {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::DegenerateFrame,
        ));
    }
    let Some(normal) = robust_unit(cross) else {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::DegenerateFrame,
        ));
    };
    let Some(v) = robust_unit(normal.cross(u)) else {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::DegenerateFrame,
        ));
    };
    Ok(Frame {
        origin: Point3::new(frame.origin.x, frame.origin.y, frame.origin.z),
        u,
        v,
        normal,
    })
}

fn robust_unit(vector: Vector3) -> Option<Vector3> {
    let scale = vector.x.abs().max(vector.y.abs()).max(vector.z.abs());
    if !scale.is_finite() || scale == 0.0 {
        return None;
    }
    let scaled = vector / scale;
    let length = scaled.length();
    (length.is_finite() && length > 0.0).then_some(scaled / length)
}

pub(crate) fn parse_loop(
    profile_loop: &PlanarLoop2,
    minimum: f64,
    agreement: f64,
) -> Result<AnalyticLoop, PlanarProfileInputError> {
    if profile_loop.curves.is_empty() {
        return Err(PlanarProfileInputError::EmptyLoop);
    }
    if profile_loop
        .curves
        .iter()
        .any(|curve| matches!(curve, PlanarCurve2::Circle { .. }))
    {
        if profile_loop.curves.len() != 1 {
            return Err(PlanarProfileInputError::DisconnectedLoop);
        }
        let PlanarCurve2::Circle {
            center,
            radius,
            direction,
        } = profile_loop.curves[0]
        else {
            unreachable!();
        };
        if radius <= minimum {
            return Err(PlanarProfileInputError::Extrusion(
                ExtrusionInputError::FeatureTooSmall,
            ));
        }
        let center = Point2::new(center.x, center.y);
        let sign = direction_sign(direction);
        let positive = Point2::new(center.x + radius, center.y);
        let negative = Point2::new(center.x - radius, center.y);
        let segments = vec![
            Segment::Arc {
                center,
                start: positive,
                end: negative,
                radius,
                start_angle: 0.0,
                sweep: sign * std::f64::consts::PI,
            },
            Segment::Arc {
                center,
                start: negative,
                end: positive,
                radius,
                start_angle: sign * std::f64::consts::PI,
                sweep: sign * std::f64::consts::PI,
            },
        ];
        let signed_area = sign * std::f64::consts::PI * radius * radius;
        return Ok(AnalyticLoop {
            segments,
            signed_area,
        });
    }

    let mut segments = Vec::with_capacity(profile_loop.curves.len());
    for curve in &profile_loop.curves {
        let segment = match *curve {
            PlanarCurve2::Line { start, end } => {
                let start = Point2::new(start.x, start.y);
                let end = Point2::new(end.x, end.y);
                if (end.x - start.x).hypot(end.y - start.y) <= minimum {
                    return Err(PlanarProfileInputError::Extrusion(
                        ExtrusionInputError::FeatureTooSmall,
                    ));
                }
                Segment::Line { start, end }
            }
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let center = Point2::new(center.x, center.y);
                let start = Point2::new(start.x, start.y);
                let end = Point2::new(end.x, end.y);
                let start_radius = (start.x - center.x).hypot(start.y - center.y);
                let end_radius = (end.x - center.x).hypot(end.y - center.y);
                if start_radius <= minimum || end_radius <= minimum {
                    return Err(PlanarProfileInputError::Extrusion(
                        ExtrusionInputError::FeatureTooSmall,
                    ));
                }
                if (start_radius - end_radius).abs() > agreement {
                    return Err(PlanarProfileInputError::Extrusion(
                        ExtrusionInputError::NumericallyIndeterminate,
                    ));
                }
                let radius = 0.5 * (start_radius + end_radius);
                let start_angle = (start.y - center.y).atan2(start.x - center.x);
                let end_angle = (end.y - center.y).atan2(end.x - center.x);
                let sweep = directed_sweep(start_angle, end_angle, direction);
                if !sweep.is_finite()
                    || sweep.abs() <= agreement / radius
                    || sweep.abs() >= std::f64::consts::TAU
                    || radius * sweep.abs() <= minimum
                {
                    return Err(PlanarProfileInputError::Extrusion(
                        ExtrusionInputError::FeatureTooSmall,
                    ));
                }
                Segment::Arc {
                    center,
                    start,
                    end,
                    radius,
                    start_angle,
                    sweep,
                }
            }
            PlanarCurve2::Circle { .. } => unreachable!(),
            PlanarCurve2::Bspline { .. } => {
                return Err(PlanarProfileInputError::AnalyticCurve);
            }
        };
        segments.push(segment);
    }
    if (0..segments.len())
        .any(|index| segments[index].end() != segments[(index + 1) % segments.len()].start())
    {
        return Err(PlanarProfileInputError::DisconnectedLoop);
    }
    if segments.len() < 2 {
        return Err(PlanarProfileInputError::DisconnectedLoop);
    }
    for first in 0..segments.len() {
        for second in first + 1..segments.len() {
            let adjacent = second == first + 1 || (first == 0 && second + 1 == segments.len());
            let invalid_contact = if adjacent {
                let mut allowed = vec![if second == first + 1 {
                    segments[first].end()
                } else {
                    segments[first].start()
                }];
                if segments.len() == 2 {
                    allowed.push(segments[first].start());
                }
                adjacent_has_extra_contact(segments[first], segments[second], &allowed, agreement)
            } else {
                segment_clearance(segments[first], segments[second]) <= minimum
            };
            if invalid_contact {
                return Err(PlanarProfileInputError::Extrusion(
                    ExtrusionInputError::SelfIntersecting,
                ));
            }
        }
    }
    let anchor = segments[0].start();
    let signed_area = segments
        .iter()
        .map(|segment| segment.translated(anchor).signed_area_contribution())
        .sum::<f64>();
    if !signed_area.is_finite() || segments.iter().any(|segment| !segment.length().is_finite()) {
        return Err(PlanarProfileInputError::Extrusion(
            ExtrusionInputError::NumericallyIndeterminate,
        ));
    }
    Ok(AnalyticLoop {
        segments,
        signed_area,
    })
}

const fn direction_sign(direction: ArcDirection) -> f64 {
    match direction {
        ArcDirection::CounterClockwise => 1.0,
        ArcDirection::Clockwise => -1.0,
    }
}

fn directed_sweep(start: f64, end: f64, direction: ArcDirection) -> f64 {
    match direction {
        ArcDirection::CounterClockwise => (end - start).rem_euclid(std::f64::consts::TAU),
        ArcDirection::Clockwise => -(start - end).rem_euclid(std::f64::consts::TAU),
    }
}

fn validate_hole_nesting(
    loops: &[AnalyticLoop],
    minimum: f64,
) -> Result<(), PlanarProfileInputError> {
    if loops.len() <= 1 {
        return Ok(());
    }
    for (hole_index, hole) in loops[1..].iter().enumerate() {
        if !point_inside_loop(hole.segments[0].start(), &loops[0])
            || loops[0].segments.iter().any(|outer| {
                hole.segments
                    .iter()
                    .any(|inner| segment_clearance(*outer, *inner) <= minimum)
            })
        {
            return Err(PlanarProfileInputError::OverlappingRegions);
        }
        for other in &loops[1..1 + hole_index] {
            if hole.segments.iter().any(|first| {
                other
                    .segments
                    .iter()
                    .any(|second| segment_clearance(*first, *second) <= minimum)
            }) || point_inside_loop(hole.segments[0].start(), other)
                || point_inside_loop(other.segments[0].start(), hole)
            {
                return Err(PlanarProfileInputError::OverlappingRegions);
            }
        }
    }
    Ok(())
}

fn point_segment_distance(point: Point2, segment: Segment) -> f64 {
    match segment {
        Segment::Line { start, end } => {
            let direction = end - start;
            let length_squared = direction.x * direction.x + direction.y * direction.y;
            let offset = point - start;
            let parameter = ((offset.x * direction.x + offset.y * direction.y) / length_squared)
                .clamp(0.0, 1.0);
            let closest = Point2::new(
                direction.x.mul_add(parameter, start.x),
                direction.y.mul_add(parameter, start.y),
            );
            (point.x - closest.x).hypot(point.y - closest.y)
        }
        Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep,
        } => {
            let offset = point - center;
            let angle = offset.y.atan2(offset.x);
            if angle_on_arc(angle, start_angle, sweep, 64.0 * f64::EPSILON) {
                (offset.x.hypot(offset.y) - radius).abs()
            } else {
                (point.x - start.x)
                    .hypot(point.y - start.y)
                    .min((point.x - end.x).hypot(point.y - end.y))
            }
        }
    }
}

/// Recovers a planar loop as exact `Segment`s from its coedge pcurves.
/// Callers that rebuild a committed profile (prism edge finishes, section
/// extraction) share this rather than re-deriving carriers per path.
pub(crate) fn topology_loop_segments(
    topology: &Topology,
    loop_key: LoopKey,
) -> Option<Vec<Segment>> {
    let profile_loop = topology.loop_record(loop_key)?;
    profile_loop
        .value
        .coedges
        .iter()
        .map(|coedge_key| {
            let coedge = &topology.coedge(*coedge_key)?.value;
            let start = coedge.pcurve.evaluate(coedge.parameter_range.start);
            let end = coedge.pcurve.evaluate(coedge.parameter_range.end);
            match coedge.pcurve {
                // A harmonic is a trace on a cylinder, never a planar profile piece.
                Curve2::Harmonic { .. } => None,
                Curve2::Line { .. } => Some(Segment::Line { start, end }),
                Curve2::Circle {
                    center,
                    u,
                    v,
                    radius,
                } => {
                    let determinant = u.x * v.y - u.y * v.x;
                    if determinant == 0.0 {
                        return None;
                    }
                    Some(Segment::Arc {
                        center,
                        start,
                        end,
                        radius,
                        start_angle: (start.y - center.y).atan2(start.x - center.x),
                        sweep: (coedge.parameter_range.end - coedge.parameter_range.start)
                            * determinant.signum(),
                    })
                }
            }
        })
        .collect()
}

pub(crate) fn segment_clearance(first: Segment, second: Segment) -> f64 {
    if segments_intersect(first, second, 0.0) {
        return 0.0;
    }
    let mut minimum = [
        point_segment_distance(first.start(), second),
        point_segment_distance(first.end(), second),
        point_segment_distance(second.start(), first),
        point_segment_distance(second.end(), first),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    match (first, second) {
        (Segment::Line { start, end }, arc @ Segment::Arc { .. })
        | (arc @ Segment::Arc { .. }, Segment::Line { start, end }) => {
            let Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            } = arc
            else {
                unreachable!();
            };
            let direction = end - start;
            let length_squared = direction.x * direction.x + direction.y * direction.y;
            let offset = center - start;
            let parameter = (offset.x * direction.x + offset.y * direction.y) / length_squared;
            if (0.0..=1.0).contains(&parameter) {
                let closest = Point2::new(
                    direction.x.mul_add(parameter, start.x),
                    direction.y.mul_add(parameter, start.y),
                );
                let radial = closest - center;
                let angle = radial.y.atan2(radial.x);
                if angle_on_arc(angle, start_angle, sweep, 64.0 * f64::EPSILON) {
                    minimum = minimum.min((radial.x.hypot(radial.y) - radius).abs());
                }
            }
        }
        (
            Segment::Arc {
                center: first_center,
                radius: first_radius,
                start_angle: first_start,
                sweep: first_sweep,
                ..
            },
            Segment::Arc {
                center: second_center,
                radius: second_radius,
                start_angle: second_start,
                sweep: second_sweep,
                ..
            },
        ) => {
            let base = (second_center.y - first_center.y).atan2(second_center.x - first_center.x);
            for first_angle in [base, base + std::f64::consts::PI] {
                if !angle_on_arc(first_angle, first_start, first_sweep, 64.0 * f64::EPSILON) {
                    continue;
                }
                let first_point = Point2::new(
                    first_radius.mul_add(first_angle.cos(), first_center.x),
                    first_radius.mul_add(first_angle.sin(), first_center.y),
                );
                for second_angle in [base, base + std::f64::consts::PI] {
                    if angle_on_arc(
                        second_angle,
                        second_start,
                        second_sweep,
                        64.0 * f64::EPSILON,
                    ) {
                        let second_point = Point2::new(
                            second_radius.mul_add(second_angle.cos(), second_center.x),
                            second_radius.mul_add(second_angle.sin(), second_center.y),
                        );
                        minimum = minimum.min(
                            (first_point.x - second_point.x).hypot(first_point.y - second_point.y),
                        );
                    }
                }
            }
        }
        (Segment::Line { .. }, Segment::Line { .. }) => {}
    }
    minimum
}

fn validate_disjoint_regions(
    regions: &[ValidatedAnalyticRegionExtrusion],
    minimum: f64,
) -> Result<(), PlanarProfileInputError> {
    for left in 0..regions.len() {
        for right in left + 1..regions.len() {
            if regions[left].loops.iter().any(|first| {
                regions[right].loops.iter().any(|second| {
                    first.segments.iter().any(|left| {
                        second
                            .segments
                            .iter()
                            .any(|right| segment_clearance(*left, *right) <= minimum)
                    })
                })
            }) || point_in_material(
                regions[left].loops[0].segments[0].start(),
                &regions[right].loops,
            ) || point_in_material(
                regions[right].loops[0].segments[0].start(),
                &regions[left].loops,
            ) {
                return Err(PlanarProfileInputError::OverlappingRegions);
            }
        }
    }
    Ok(())
}

pub(crate) fn point_in_material(point: Point2, loops: &[AnalyticLoop]) -> bool {
    point_inside_loop(point, &loops[0])
        && loops[1..]
            .iter()
            .all(|hole| !point_inside_loop(point, hole))
}

pub(crate) fn point_inside_loop(point: Point2, profile_loop: &AnalyticLoop) -> bool {
    let mut crossings = 0_usize;
    for segment in &profile_loop.segments {
        match *segment {
            Segment::Line { start, end } => {
                if (start.y > point.y) != (end.y > point.y) {
                    let x =
                        (end.x - start.x).mul_add((point.y - start.y) / (end.y - start.y), start.x);
                    if x > point.x {
                        crossings += 1;
                    }
                }
            }
            arc @ Segment::Arc { .. } => {
                // Split the arc into y-monotone pieces at its interior sine
                // extremes and apply the same endpoint-straddle rule as the
                // line arm. The outer piece endpoints are the segment's own
                // stored points — bit-exact with its neighbours' — so a ray
                // through a shared seam vertex counts exactly as it would at
                // a polygon vertex, instead of falling into the measure-zero
                // angular gap an ulp-shifted seam leaves between two arcs.
                for (from, to, rightward) in monotone_arc_pieces(arc) {
                    if (from.y > point.y) != (to.y > point.y) {
                        let Segment::Arc { center, radius, .. } = arc else {
                            unreachable!()
                        };
                        let offset = point.y - center.y;
                        let square = radius.mul_add(radius, -(offset * offset)).max(0.0);
                        let x = if rightward {
                            center.x + square.sqrt()
                        } else {
                            center.x - square.sqrt()
                        };
                        if x > point.x {
                            crossings += 1;
                        }
                    }
                }
            }
        }
    }
    crossings % 2 == 1
}

/// The y-monotone pieces of an arc as `(from, to, rightward)` endpoint
/// pairs, split at the interior angles where the sine is extremal. The
/// original endpoints keep the segment's stored points bit for bit;
/// interior split points take the exact extreme ordinate `center.y ± r`, so
/// straddle comparisons at the extremes are exact as well. `rightward` says
/// which x-half of the circle the piece lies in.
fn monotone_arc_pieces(arc: Segment) -> Vec<(Point2, Point2, bool)> {
    let Segment::Arc {
        center,
        start,
        end,
        radius,
        start_angle,
        sweep,
    } = arc
    else {
        return Vec::new();
    };
    // Interior sine extremes: angles congruent to ±π/2 strictly inside the
    // swept range, in traversal order.
    let half = std::f64::consts::FRAC_PI_2;
    let tau = std::f64::consts::TAU;
    let mut cuts: Vec<f64> = Vec::new();
    let direction = if sweep >= 0.0 { 1.0 } else { -1.0 };
    let span = sweep.abs();
    for base in [half, -half] {
        // First congruent value strictly after the start in sweep direction.
        let offset = (direction * (base - start_angle)).rem_euclid(tau);
        let mut along = if offset == 0.0 { tau } else { offset };
        while along < span {
            cuts.push(along);
            along += tau;
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();

    let point_at = |along: f64| {
        let angle = start_angle + direction * along;
        // Snap the ordinate to the exact extreme so straddle tests at the
        // split are exact; the abscissa is only used for crossing x, which
        // is recomputed per query anyway.
        let sine = angle.sin();
        let y = if (sine - 1.0).abs() < 1.0e-9 {
            center.y + radius
        } else if (sine + 1.0).abs() < 1.0e-9 {
            center.y - radius
        } else {
            radius.mul_add(sine, center.y)
        };
        Point2::new(radius.mul_add(angle.cos(), center.x), y)
    };
    let mut boundaries = Vec::with_capacity(cuts.len() + 2);
    boundaries.push((0.0, start));
    for along in cuts {
        boundaries.push((along, point_at(along)));
    }
    boundaries.push((span, end));

    boundaries
        .windows(2)
        .map(|pair| {
            let (from_along, from) = pair[0];
            let (to_along, to) = pair[1];
            let middle_angle = start_angle + direction * ((from_along + to_along) / 2.0);
            (from, to, middle_angle.cos() > 0.0)
        })
        .collect()
}

fn arc_progress(angle: f64, start: f64, sweep: f64) -> f64 {
    if sweep > 0.0 {
        (angle - start).rem_euclid(std::f64::consts::TAU) / sweep
    } else {
        (start - angle).rem_euclid(std::f64::consts::TAU) / -sweep
    }
}

fn angle_on_arc(angle: f64, start: f64, sweep: f64, tolerance: f64) -> bool {
    let progress = arc_progress(angle, start, sweep);
    progress >= -tolerance && progress <= 1.0 + tolerance
}

fn segments_intersect(first: Segment, second: Segment, tolerance: f64) -> bool {
    match (first, second) {
        (
            Segment::Line {
                start: first_start,
                end: first_end,
            },
            Segment::Line {
                start: second_start,
                end: second_end,
            },
        ) => line_segments_intersect(first_start, first_end, second_start, second_end, tolerance),
        (Segment::Line { start, end }, arc @ Segment::Arc { .. })
        | (arc @ Segment::Arc { .. }, Segment::Line { start, end }) => {
            line_arc_intersect(start, end, arc, tolerance)
        }
        (first @ Segment::Arc { .. }, second @ Segment::Arc { .. }) => {
            arcs_intersect(first, second, tolerance)
        }
    }
}

fn adjacent_has_extra_contact(
    first: Segment,
    second: Segment,
    allowed: &[Point2],
    tolerance: f64,
) -> bool {
    let away_from_allowed = |point: Point2| {
        allowed
            .iter()
            .all(|allowed| (point.x - allowed.x).hypot(point.y - allowed.y) > tolerance)
    };
    match (first, second) {
        (Segment::Line { .. }, Segment::Line { .. }) => {
            [first.start(), first.end(), second.start(), second.end()]
                .into_iter()
                .filter(|point| away_from_allowed(*point))
                .any(|point| {
                    point_segment_distance(point, first) <= tolerance
                        && point_segment_distance(point, second) <= tolerance
                })
        }
        (Segment::Line { start, end }, arc @ Segment::Arc { .. })
        | (arc @ Segment::Arc { .. }, Segment::Line { start, end }) => {
            line_arc_intersection_points(start, end, arc, tolerance)
                .into_iter()
                .any(away_from_allowed)
        }
        (first @ Segment::Arc { .. }, second @ Segment::Arc { .. }) => {
            if coincident_arcs_overlap(first, second, tolerance) {
                return true;
            }
            arc_arc_intersection_points(first, second, tolerance)
                .into_iter()
                .any(away_from_allowed)
        }
    }
}

fn line_arc_intersection_points(
    start: Point2,
    end: Point2,
    arc: Segment,
    tolerance: f64,
) -> Vec<Point2> {
    let Segment::Arc {
        center,
        radius,
        start_angle,
        sweep,
        ..
    } = arc
    else {
        return Vec::new();
    };
    let direction = end - start;
    let length = direction.x.hypot(direction.y);
    let offset = start - center;
    let a = length * length;
    let b = 2.0 * (offset.x * direction.x + offset.y * direction.y);
    let c = offset.x * offset.x + offset.y * offset.y - radius * radius;
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 || !discriminant.is_finite() {
        return Vec::new();
    }
    let root = discriminant.sqrt();
    let parameter_tolerance = tolerance / length;
    let mut points = Vec::new();
    for signed in [-root, root] {
        let parameter = (-b + signed) / (2.0 * a);
        if parameter < -parameter_tolerance || parameter > 1.0 + parameter_tolerance {
            continue;
        }
        let point = Point2::new(
            direction.x.mul_add(parameter, start.x),
            direction.y.mul_add(parameter, start.y),
        );
        let angle = (point.y - center.y).atan2(point.x - center.x);
        if angle_on_arc(angle, start_angle, sweep, tolerance / radius)
            && points
                .iter()
                .all(|other: &Point2| (point.x - other.x).hypot(point.y - other.y) > tolerance)
        {
            points.push(point);
        }
    }
    points
}

fn coincident_arcs_overlap(first: Segment, second: Segment, tolerance: f64) -> bool {
    let Segment::Arc {
        center: first_center,
        radius: first_radius,
        start_angle: first_start,
        sweep: first_sweep,
        ..
    } = first
    else {
        return false;
    };
    let Segment::Arc {
        center: second_center,
        radius: second_radius,
        start_angle: second_start,
        sweep: second_sweep,
        ..
    } = second
    else {
        return false;
    };
    if (first_center.x - second_center.x).hypot(first_center.y - second_center.y) > tolerance
        || (first_radius - second_radius).abs() > tolerance
    {
        return false;
    }
    let angular_tolerance = tolerance / first_radius.max(second_radius);
    [
        first_start,
        first_start + first_sweep,
        first_start + 0.5 * first_sweep,
    ]
    .into_iter()
    .any(|angle| {
        let progress = arc_progress(angle, second_start, second_sweep);
        progress > angular_tolerance && progress < 1.0 - angular_tolerance
    }) || [
        second_start,
        second_start + second_sweep,
        second_start + 0.5 * second_sweep,
    ]
    .into_iter()
    .any(|angle| {
        let progress = arc_progress(angle, first_start, first_sweep);
        progress > angular_tolerance && progress < 1.0 - angular_tolerance
    })
}

fn arc_arc_intersection_points(first: Segment, second: Segment, tolerance: f64) -> Vec<Point2> {
    let Segment::Arc {
        center: first_center,
        radius: first_radius,
        start_angle: first_start,
        sweep: first_sweep,
        ..
    } = first
    else {
        return Vec::new();
    };
    let Segment::Arc {
        center: second_center,
        radius: second_radius,
        start_angle: second_start,
        sweep: second_sweep,
        ..
    } = second
    else {
        return Vec::new();
    };
    let dx = second_center.x - first_center.x;
    let dy = second_center.y - first_center.y;
    let distance = dx.hypot(dy);
    if distance <= tolerance
        || distance > first_radius + second_radius + tolerance
        || distance < (first_radius - second_radius).abs() - tolerance
    {
        return Vec::new();
    }
    let along = (first_radius * first_radius - second_radius * second_radius + distance * distance)
        / (2.0 * distance);
    let height_squared = first_radius * first_radius - along * along;
    if height_squared < 0.0 {
        return Vec::new();
    }
    let height = height_squared.sqrt();
    let base = Point2::new(
        first_center.x + along * dx / distance,
        first_center.y + along * dy / distance,
    );
    let mut points = Vec::new();
    for sign in [1.0, -1.0] {
        let point = Point2::new(
            base.x - sign * height * dy / distance,
            base.y + sign * height * dx / distance,
        );
        let first_angle = (point.y - first_center.y).atan2(point.x - first_center.x);
        let second_angle = (point.y - second_center.y).atan2(point.x - second_center.x);
        if angle_on_arc(
            first_angle,
            first_start,
            first_sweep,
            tolerance / first_radius,
        ) && angle_on_arc(
            second_angle,
            second_start,
            second_sweep,
            tolerance / second_radius,
        ) && points
            .iter()
            .all(|other: &Point2| (point.x - other.x).hypot(point.y - other.y) > tolerance)
        {
            points.push(point);
        }
    }
    points
}

fn line_segments_intersect(
    first_start: Point2,
    first_end: Point2,
    second_start: Point2,
    second_end: Point2,
    tolerance: f64,
) -> bool {
    let first = first_end - first_start;
    let second = second_end - second_start;
    let offset = second_start - first_start;
    let denominator = first.x * second.y - first.y * second.x;
    if denominator.abs() <= f64::EPSILON * first.x.hypot(first.y) * second.x.hypot(second.y) {
        let collinear =
            (offset.x * first.y - offset.y * first.x).abs() <= tolerance * first.x.hypot(first.y);
        if !collinear {
            return false;
        }
        return first_start.x.min(first_end.x) <= second_start.x.max(second_end.x) + tolerance
            && second_start.x.min(second_end.x) <= first_start.x.max(first_end.x) + tolerance
            && first_start.y.min(first_end.y) <= second_start.y.max(second_end.y) + tolerance
            && second_start.y.min(second_end.y) <= first_start.y.max(first_end.y) + tolerance;
    }
    let t = (offset.x * second.y - offset.y * second.x) / denominator;
    let u = (offset.x * first.y - offset.y * first.x) / denominator;
    let first_parameter_tolerance = tolerance / first.x.hypot(first.y);
    let second_parameter_tolerance = tolerance / second.x.hypot(second.y);
    t >= -first_parameter_tolerance
        && t <= 1.0 + first_parameter_tolerance
        && u >= -second_parameter_tolerance
        && u <= 1.0 + second_parameter_tolerance
}

fn line_arc_intersect(start: Point2, end: Point2, arc: Segment, tolerance: f64) -> bool {
    !line_arc_intersection_points(start, end, arc, tolerance).is_empty()
}

fn arcs_intersect(first: Segment, second: Segment, tolerance: f64) -> bool {
    coincident_arcs_overlap(first, second, tolerance)
        || !arc_arc_intersection_points(first, second, tolerance).is_empty()
}

#[derive(Clone, Debug)]
struct LoopKeys {
    bottom_vertices: Vec<VertexKey>,
    top_vertices: Vec<VertexKey>,
    bottom_edges: Vec<EdgeKey>,
    top_edges: Vec<EdgeKey>,
    vertical_edges: Vec<EdgeKey>,
}

pub(crate) fn build_analytic_extrusion(extrusion: &ValidatedAnalyticExtrusion) -> Topology {
    merge_topologies(
        extrusion
            .regions
            .iter()
            .map(build_analytic_region)
            .collect(),
    )
}

fn build_analytic_region(extrusion: &ValidatedAnalyticRegionExtrusion) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let mut loop_keys = Vec::with_capacity(extrusion.loops.len());

    for profile_loop in &extrusion.loops {
        let mut keys = LoopKeys {
            bottom_vertices: Vec::with_capacity(profile_loop.segments.len()),
            top_vertices: Vec::with_capacity(profile_loop.segments.len()),
            bottom_edges: Vec::with_capacity(profile_loop.segments.len()),
            top_edges: Vec::with_capacity(profile_loop.segments.len()),
            vertical_edges: Vec::with_capacity(profile_loop.segments.len()),
        };
        for segment in &profile_loop.segments {
            keys.bottom_vertices.push(push_vertex(
                &mut topology,
                &mut next_id,
                extrusion.frame.point(segment.start(), 0.0),
            ));
        }
        for segment in &profile_loop.segments {
            keys.top_vertices.push(push_vertex(
                &mut topology,
                &mut next_id,
                extrusion.frame.point(segment.start(), extrusion.distance),
            ));
        }
        let count = profile_loop.segments.len();
        for (index, segment) in profile_loop.segments.iter().copied().enumerate() {
            let next = (index + 1) % count;
            keys.bottom_edges.push(push_boundary_edge(
                &mut topology,
                &mut next_id,
                [keys.bottom_vertices[index], keys.bottom_vertices[next]],
                segment,
                extrusion.frame,
                0.0,
            ));
        }
        for (index, segment) in profile_loop.segments.iter().copied().enumerate() {
            let next = (index + 1) % count;
            keys.top_edges.push(push_boundary_edge(
                &mut topology,
                &mut next_id,
                [keys.top_vertices[index], keys.top_vertices[next]],
                segment,
                extrusion.frame,
                extrusion.distance,
            ));
        }
        for index in 0..count {
            let start = topology.vertices[keys.bottom_vertices[index].0].value.point;
            let end = topology.vertices[keys.top_vertices[index].0].value.point;
            keys.vertical_edges.push(push_edge(
                &mut topology,
                &mut next_id,
                Edge::line(
                    [keys.bottom_vertices[index], keys.top_vertices[index]],
                    [start, end],
                ),
            ));
        }
        loop_keys.push(keys);
    }

    let bottom_loops = extrusion
        .loops
        .iter()
        .zip(&loop_keys)
        .map(|(profile_loop, keys)| {
            let uses = (0..profile_loop.segments.len())
                .rev()
                .map(|index| BoundaryUse {
                    edge: keys.bottom_edges[index],
                    orientation: Orientation::Reverse,
                    curve: cap_pcurve(profile_loop.segments[index], true, true),
                })
                .collect::<Vec<_>>();
            push_loop(&mut topology, &mut next_id, uses)
        })
        .collect::<Vec<_>>();
    push_cap_face(
        &mut topology,
        &mut next_id,
        Surface::Plane(Plane::new(
            extrusion.frame.origin,
            extrusion.frame.v,
            extrusion.frame.u,
        )),
        &bottom_loops,
        FaceRole::ExtrusionBottom,
    );

    let top_loops = extrusion
        .loops
        .iter()
        .zip(&loop_keys)
        .map(|(profile_loop, keys)| {
            let uses = profile_loop
                .segments
                .iter()
                .copied()
                .enumerate()
                .map(|(index, segment)| BoundaryUse {
                    edge: keys.top_edges[index],
                    orientation: Orientation::Forward,
                    curve: cap_pcurve(segment, false, false),
                })
                .collect::<Vec<_>>();
            push_loop(&mut topology, &mut next_id, uses)
        })
        .collect::<Vec<_>>();
    push_cap_face(
        &mut topology,
        &mut next_id,
        Surface::Plane(Plane::new(
            extrusion.frame.origin + extrusion.frame.normal * extrusion.distance,
            extrusion.frame.u,
            extrusion.frame.v,
        )),
        &top_loops,
        FaceRole::ExtrusionTop,
    );

    let mut side_ordinal = 0_u32;
    for (profile_loop, keys) in extrusion.loops.iter().zip(&loop_keys) {
        let count = profile_loop.segments.len();
        for (index, segment) in profile_loop.segments.iter().copied().enumerate() {
            let next = (index + 1) % count;
            push_side_face(
                &mut topology,
                &mut next_id,
                extrusion,
                segment,
                [
                    keys.bottom_edges[index],
                    keys.vertical_edges[next],
                    keys.top_edges[index],
                    keys.vertical_edges[index],
                ],
                FaceRole::ExtrusionSide(side_ordinal),
            );
            side_ordinal += 1;
        }
    }

    let shell_key = ShellKey(topology.shells.len());
    topology.shells.push(Record {
        id: allocate_id(&mut next_id),
        value: Shell {
            faces: (0..topology.faces.len()).map(FaceKey).collect(),
        },
    });
    topology.solids.push(Record {
        id: allocate_id(&mut next_id),
        value: Solid {
            outer_shell: shell_key,
            inner_shells: Vec::new(),
        },
    });
    topology
}

pub(crate) fn push_vertex(topology: &mut Topology, next_id: &mut u64, point: Point3) -> VertexKey {
    let key = VertexKey(topology.vertices.len());
    topology.vertices.push(Record {
        id: allocate_id(next_id),
        value: Vertex { point },
    });
    key
}

pub(crate) fn push_edge(topology: &mut Topology, next_id: &mut u64, edge: Edge) -> EdgeKey {
    let key = EdgeKey(topology.edges.len());
    topology.edges.push(Record {
        id: allocate_id(next_id),
        value: edge,
    });
    key
}

pub(crate) fn push_boundary_edge(
    topology: &mut Topology,
    next_id: &mut u64,
    vertices: [VertexKey; 2],
    segment: Segment,
    frame: Frame,
    height: f64,
) -> EdgeKey {
    let edge = match segment {
        Segment::Line { start, end } => Edge::line(
            vertices,
            [frame.point(start, height), frame.point(end, height)],
        ),
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => Edge {
            vertices,
            curve: Curve3::Circle {
                center: frame.center(center, height),
                u: frame.u,
                v: frame.v,
                radius,
            },
            parameter_range: ParameterRange::new(start_angle, start_angle + sweep),
        },
    };
    push_edge(topology, next_id, edge)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BoundaryUse {
    pub(crate) edge: EdgeKey,
    pub(crate) orientation: Orientation,
    pub(crate) curve: (Curve2, ParameterRange),
}

pub(crate) fn cap_pcurve(segment: Segment, swap: bool, reverse: bool) -> (Curve2, ParameterRange) {
    let map = |point: Point2| {
        if swap {
            Point2::new(point.y, point.x)
        } else {
            point
        }
    };
    match segment {
        Segment::Line { start, end } => {
            let endpoints = if reverse {
                [map(end), map(start)]
            } else {
                [map(start), map(end)]
            };
            Curve2::line_segment(endpoints)
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let u = if swap {
                Vector2::new(0.0, 1.0)
            } else {
                Vector2::new(1.0, 0.0)
            };
            let v = if swap {
                Vector2::new(1.0, 0.0)
            } else {
                Vector2::new(0.0, 1.0)
            };
            let range = ParameterRange::new(start_angle, start_angle + sweep);
            (
                Curve2::Circle {
                    center: map(center),
                    u,
                    v,
                    radius,
                },
                if reverse { range.reversed() } else { range },
            )
        }
    }
}

pub(crate) fn push_loop(
    topology: &mut Topology,
    next_id: &mut u64,
    uses: Vec<BoundaryUse>,
) -> LoopKey {
    let mut coedge_keys = Vec::with_capacity(uses.len());
    for boundary_use in uses {
        let key = CoedgeKey(topology.coedges.len());
        topology.coedges.push(Record {
            id: allocate_id(next_id),
            value: Coedge {
                edge: boundary_use.edge,
                orientation: boundary_use.orientation,
                pcurve: boundary_use.curve.0,
                parameter_range: boundary_use.curve.1,
            },
        });
        coedge_keys.push(key);
    }
    let key = LoopKey(topology.loops.len());
    topology.loops.push(Record {
        id: allocate_id(next_id),
        value: Loop {
            coedges: coedge_keys,
        },
    });
    key
}

pub(crate) fn push_cap_face(
    topology: &mut Topology,
    next_id: &mut u64,
    surface: Surface,
    loops: &[LoopKey],
    role: FaceRole,
) {
    topology.faces.push(Record {
        id: allocate_id(next_id),
        value: Face {
            surface,
            outer_loop: loops[0],
            inner_loops: loops[1..].to_vec(),
            role,
        },
    });
}

fn push_side_face(
    topology: &mut Topology,
    next_id: &mut u64,
    extrusion: &ValidatedAnalyticRegionExtrusion,
    segment: Segment,
    edges: [EdgeKey; 4],
    role: FaceRole,
) {
    let (surface, bottom, right, top, left) = match segment {
        Segment::Line { start, end } => {
            let length = (end.x - start.x).hypot(end.y - start.y);
            let tangent = extrusion.frame.u * ((end.x - start.x) / length)
                + extrusion.frame.v * ((end.y - start.y) / length);
            (
                Surface::Plane(Plane::new(
                    extrusion.frame.point(start, 0.0),
                    tangent,
                    extrusion.frame.normal,
                )),
                Curve2::line_segment([Point2::new(0.0, 0.0), Point2::new(length, 0.0)]),
                Curve2::line_segment([
                    Point2::new(length, 0.0),
                    Point2::new(length, extrusion.distance),
                ]),
                Curve2::line_segment([
                    Point2::new(length, extrusion.distance),
                    Point2::new(0.0, extrusion.distance),
                ]),
                Curve2::line_segment([Point2::new(0.0, extrusion.distance), Point2::new(0.0, 0.0)]),
            )
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let sign = sweep.signum();
            let start = sign * start_angle;
            let end = sign * (start_angle + sweep);
            (
                Surface::Cylinder(Cylinder {
                    origin: extrusion.frame.center(center, 0.0),
                    axis: extrusion.frame.normal,
                    radial_u: extrusion.frame.u,
                    radial_v: extrusion.frame.v,
                    radius,
                    angular_sign: sign,
                }),
                Curve2::line_segment([Point2::new(start, 0.0), Point2::new(end, 0.0)]),
                Curve2::line_segment([Point2::new(end, 0.0), Point2::new(end, extrusion.distance)]),
                Curve2::line_segment([
                    Point2::new(end, extrusion.distance),
                    Point2::new(start, extrusion.distance),
                ]),
                Curve2::line_segment([
                    Point2::new(start, extrusion.distance),
                    Point2::new(start, 0.0),
                ]),
            )
        }
    };
    let loop_key = push_loop(
        topology,
        next_id,
        [bottom, right, top, left]
            .into_iter()
            .zip([
                (edges[0], Orientation::Forward),
                (edges[1], Orientation::Forward),
                (edges[2], Orientation::Reverse),
                (edges[3], Orientation::Reverse),
            ])
            .map(|(curve, (edge, orientation))| BoundaryUse {
                edge,
                orientation,
                curve,
            })
            .collect(),
    );
    topology.faces.push(Record {
        id: allocate_id(next_id),
        value: Face {
            surface,
            outer_loop: loop_key,
            inner_loops: Vec::new(),
            role,
        },
    });
}

pub(crate) fn allocate_id(next_id: &mut u64) -> EntityId {
    let id = EntityId::from_raw(*next_id);
    *next_id += 1;
    id
}

pub(crate) fn merge_topologies(topologies: Vec<Topology>) -> Topology {
    let mut merged = Topology::default();
    let mut next_id = 1_u64;
    for mut topology in topologies {
        let vertex_offset = merged.vertices.len();
        let edge_offset = merged.edges.len();
        let coedge_offset = merged.coedges.len();
        let loop_offset = merged.loops.len();
        let face_offset = merged.faces.len();
        let shell_offset = merged.shells.len();

        for record in &mut topology.vertices {
            record.id = allocate_id(&mut next_id);
        }
        for record in &mut topology.edges {
            record.id = allocate_id(&mut next_id);
            record.value.vertices = record
                .value
                .vertices
                .map(|key| VertexKey(key.0 + vertex_offset));
        }
        for record in &mut topology.coedges {
            record.id = allocate_id(&mut next_id);
            record.value.edge = EdgeKey(record.value.edge.0 + edge_offset);
        }
        for record in &mut topology.loops {
            record.id = allocate_id(&mut next_id);
            for key in &mut record.value.coedges {
                *key = CoedgeKey(key.0 + coedge_offset);
            }
        }
        for record in &mut topology.faces {
            record.id = allocate_id(&mut next_id);
            record.value.outer_loop = LoopKey(record.value.outer_loop.0 + loop_offset);
            for key in &mut record.value.inner_loops {
                *key = LoopKey(key.0 + loop_offset);
            }
        }
        for record in &mut topology.shells {
            record.id = allocate_id(&mut next_id);
            for key in &mut record.value.faces {
                *key = FaceKey(key.0 + face_offset);
            }
        }
        for record in &mut topology.solids {
            record.id = allocate_id(&mut next_id);
            record.value.outer_shell = ShellKey(record.value.outer_shell.0 + shell_offset);
            for inner in &mut record.value.inner_shells {
                *inner = ShellKey(inner.0 + shell_offset);
            }
        }

        merged.vertices.extend(topology.vertices);
        merged.edges.extend(topology.edges);
        merged.coedges.extend(topology.coedges);
        merged.loops.extend(topology.loops);
        merged.faces.extend(topology.faces);
        merged.shells.extend(topology.shells);
        merged.solids.extend(topology.solids);
    }
    merged
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        ArcDirection, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
        Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy,
        Vector3 as ProtocolVector3,
    };

    use super::{build_analytic_extrusion, validate_analytic_profile_extrusion};
    use crate::topology::{Curve3, Point3};
    use crate::validator;

    fn frame() -> PlanarFrame3 {
        PlanarFrame3 {
            origin: ProtocolPoint3::new(0.0, 0.0, 0.0),
            u: ProtocolVector3::new(1.0, 0.0, 0.0),
            v: ProtocolVector3::new(0.0, 1.0, 0.0),
        }
    }

    fn circle(radius: f64, direction: ArcDirection) -> PlanarLoop2 {
        circle_at(0.0, 0.0, radius, direction)
    }

    fn circle_at(x: f64, y: f64, radius: f64, direction: ArcDirection) -> PlanarLoop2 {
        PlanarLoop2 {
            curves: vec![PlanarCurve2::Circle {
                center: ProtocolPoint2::new(x, y),
                radius,
                direction,
            }],
        }
    }

    fn build(profile: PlanarProfile2, distance: f64) -> crate::topology::Topology {
        let precision = PrecisionPolicy::default();
        let validated = validate_analytic_profile_extrusion(frame(), &profile, distance, precision)
            .expect("analytic profile validates");
        build_analytic_extrusion(&validated)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1.0e-9 * expected.abs().max(1.0),
            "expected {expected:.17e}, got {actual:.17e}"
        );
    }

    #[test]
    fn disk_is_two_exact_semicircles_with_cylindrical_walls() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle(2.0, ArcDirection::CounterClockwise),
                    holes: Vec::new(),
                }],
            },
            3.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_eq!(topology.vertices.len(), 4);
        assert_eq!(topology.edges.len(), 6);
        assert_eq!(topology.faces.len(), 4);
        assert_eq!(
            topology
                .edges
                .iter()
                .filter(|edge| matches!(edge.value.curve, Curve3::Circle { .. }))
                .count(),
            4
        );
        assert_close(report.measures.signed_volume, 12.0 * std::f64::consts::PI);
        assert_close(report.measures.surface_area, 20.0 * std::f64::consts::PI);
        let bounds = report.measures.bounds.expect("disk bounds");
        assert_eq!(bounds.min, Point3::new(-2.0, -2.0, 0.0));
        assert_eq!(bounds.max, Point3::new(2.0, 2.0, 3.0));
    }

    #[test]
    fn annulus_retains_exact_inner_and_outer_circles() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle(3.0, ArcDirection::CounterClockwise),
                    holes: vec![circle(1.0, ArcDirection::Clockwise)],
                }],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_eq!(topology.vertices.len(), 8);
        assert_eq!(topology.edges.len(), 12);
        assert_eq!(topology.faces.len(), 6);
        assert_close(report.measures.signed_volume, 16.0 * std::f64::consts::PI);
        assert_close(report.measures.surface_area, 32.0 * std::f64::consts::PI);
    }

    #[test]
    fn mixed_line_and_arc_profile_is_authoritative() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![
                            PlanarCurve2::Line {
                                start: ProtocolPoint2::new(-1.0, 0.0),
                                end: ProtocolPoint2::new(1.0, 0.0),
                            },
                            PlanarCurve2::CircularArc {
                                center: ProtocolPoint2::new(0.0, 0.0),
                                start: ProtocolPoint2::new(1.0, 0.0),
                                end: ProtocolPoint2::new(-1.0, 0.0),
                                direction: ArcDirection::CounterClockwise,
                            },
                        ],
                    },
                    holes: Vec::new(),
                }],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_eq!(topology.faces.len(), 4);
        assert_close(report.measures.signed_volume, std::f64::consts::PI);
        assert_close(
            report.measures.surface_area,
            3.0 * std::f64::consts::PI + 4.0,
        );
    }

    #[test]
    fn rectangular_outer_with_circular_hole_is_exact() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2::from_polygon(&[
                        ProtocolPoint2::new(-4.0, -3.0),
                        ProtocolPoint2::new(4.0, -3.0),
                        ProtocolPoint2::new(4.0, 3.0),
                        ProtocolPoint2::new(-4.0, 3.0),
                    ]),
                    holes: vec![circle(1.0, ArcDirection::Clockwise)],
                }],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_close(
            report.measures.signed_volume,
            2.0 * (48.0 - std::f64::consts::PI),
        );
    }

    #[test]
    fn circular_outer_with_rectangular_hole_is_exact() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle(3.0, ArcDirection::CounterClockwise),
                    holes: vec![PlanarLoop2::from_polygon(&[
                        ProtocolPoint2::new(-1.0, -1.0),
                        ProtocolPoint2::new(-1.0, 1.0),
                        ProtocolPoint2::new(1.0, 1.0),
                        ProtocolPoint2::new(1.0, -1.0),
                    ])],
                }],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_close(
            report.measures.signed_volume,
            2.0 * (9.0 * std::f64::consts::PI - 4.0),
        );
    }

    #[test]
    fn asymmetric_disjoint_circles_aggregate_volume_and_centroid() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![
                    PlanarRegion2 {
                        outer: circle_at(-4.0, 0.0, 2.0, ArcDirection::CounterClockwise),
                        holes: Vec::new(),
                    },
                    PlanarRegion2 {
                        outer: circle_at(3.0, 0.0, 1.0, ArcDirection::CounterClockwise),
                        holes: Vec::new(),
                    },
                ],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_eq!(topology.solids.len(), 2);
        assert_close(report.measures.signed_volume, 10.0 * std::f64::consts::PI);
        let centroid = report.measures.centroid.expect("aggregate centroid");
        assert_close(centroid.x, -2.6);
        assert_close(centroid.y, 0.0);
        assert_close(centroid.z, 1.0);
    }

    #[test]
    fn annulus_void_accepts_disjoint_depth_two_circle_island() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![
                    PlanarRegion2 {
                        outer: circle(3.0, ArcDirection::CounterClockwise),
                        holes: vec![circle(2.0, ArcDirection::Clockwise)],
                    },
                    PlanarRegion2 {
                        outer: circle(1.0, ArcDirection::CounterClockwise),
                        holes: Vec::new(),
                    },
                ],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_eq!(topology.solids.len(), 2);
        assert_close(report.measures.signed_volume, 12.0 * std::f64::consts::PI);
    }

    #[test]
    fn disjoint_analytic_regions_below_minimum_clearance_are_rejected() {
        let profile = PlanarProfile2 {
            regions: vec![
                PlanarRegion2 {
                    outer: circle_at(0.0, 0.0, 1.0, ArcDirection::CounterClockwise),
                    holes: Vec::new(),
                },
                PlanarRegion2 {
                    outer: circle_at(
                        2.0 + 0.5 * PrecisionPolicy::default().min_feature_size,
                        0.0,
                        1.0,
                        ArcDirection::CounterClockwise,
                    ),
                    holes: Vec::new(),
                },
            ],
        };
        assert!(matches!(
            validate_analytic_profile_extrusion(frame(), &profile, 2.0, PrecisionPolicy::default()),
            Err(crate::planar_profile::PlanarProfileInputError::OverlappingRegions)
        ));
    }

    #[test]
    fn adjacent_arcs_with_a_second_contact_are_rejected() {
        let root_three = 3.0_f64.sqrt();
        let profile = PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![
                        PlanarCurve2::CircularArc {
                            center: ProtocolPoint2::new(-1.0, 0.0),
                            start: ProtocolPoint2::new(-3.0, 0.0),
                            end: ProtocolPoint2::new(0.0, root_three),
                            direction: ArcDirection::Clockwise,
                        },
                        PlanarCurve2::CircularArc {
                            center: ProtocolPoint2::new(1.0, 0.0),
                            start: ProtocolPoint2::new(0.0, root_three),
                            end: ProtocolPoint2::new(3.0, 0.0),
                            direction: ArcDirection::CounterClockwise,
                        },
                        PlanarCurve2::Line {
                            start: ProtocolPoint2::new(3.0, 0.0),
                            end: ProtocolPoint2::new(-3.0, 0.0),
                        },
                    ],
                },
                holes: Vec::new(),
            }],
        };
        assert!(matches!(
            validate_analytic_profile_extrusion(frame(), &profile, 2.0, PrecisionPolicy::default()),
            Err(crate::planar_profile::PlanarProfileInputError::Extrusion(
                crate::extrusion::ExtrusionInputError::SelfIntersecting
            ))
        ));
    }

    #[test]
    fn translated_circle_area_and_moments_remain_exact_near_coordinate_limit() {
        let topology = build(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle_at(
                        999_999_900.0,
                        999_999_900.0,
                        2.0,
                        ArcDirection::CounterClockwise,
                    ),
                    holes: Vec::new(),
                }],
            },
            2.0,
        );
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_close(report.measures.signed_volume, 8.0 * std::f64::consts::PI);
        let centroid = report.measures.centroid.expect("translated centroid");
        assert_close(centroid.x, 999_999_900.0);
        assert_close(centroid.y, 999_999_900.0);
    }
}

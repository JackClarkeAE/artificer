//! The read-only queries CAM (ADR 0057) reads off a snapshot.
//!
//! CAM lives outside this crate and never constructs topology. What it needs
//! is what the kernel already computes for its own blends and Booleans: the
//! `(r, z)` section of a solid of revolution, the profile of a prism, an
//! exact point-in-solid answer, the certified mitred offset of a planar loop,
//! and the exact regularized Boolean of planar regions. Each function here is
//! a thin public face over one of those, speaking the protocol's planar
//! vocabulary so nothing private crosses the crate boundary.

use std::fmt;

use artificer_protocol::{
    ArcDirection, BooleanOperation, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarRegion2,
    Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy,
    Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::{Segment, parse_curve};
use crate::loop_offset::{LoopOffsetError, ReflexPolicy, mitred_offset};
use crate::prism_edge_finish::extract_prism;
use crate::profile_boolean::{ProfileBooleanError, ProfileRegion, profile_boolean_multi};
use crate::section_revolve::extract_rz_section;
use crate::topology::{Point2, Point3, Vector3};
use crate::{NativeKernel, Snapshot, protocol_point, protocol_vector};

/// The `(r, z)` section of a coaxial solid of revolution, as CAM reads it.
///
/// `curves` run counter-clockwise with `r` as `x` and `z` as `y`, so material
/// lies on their left. A section that is not `closed` starts and ends on the
/// axis (`r = 0`) and is closed by the implicit axis segment from its last
/// point back to its first; a closed one is a tube's section, clear of the
/// axis. Heights are measured from `center` along `axis`; azimuth zero is
/// `radial`.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnedSection {
    pub center: ProtocolPoint3,
    pub axis: ProtocolVector3,
    pub radial: ProtocolVector3,
    pub curves: Vec<PlanarCurve2>,
    pub closed: bool,
    /// How far the solid turns: a full turn, or less for a partial revolve.
    pub sweep: f64,
}

/// A prism as CAM reads it: the frame of its bottom cap, its height along the
/// frame normal, and its profile loops in that frame, the outer loop
/// counter-clockwise and every hole clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct PrismProfile {
    pub frame: PlanarFrame3,
    pub height: f64,
    pub outer: PlanarLoop2,
    pub holes: Vec<PlanarLoop2>,
}

/// Why a CAM query could not answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CamQueryError {
    /// The loop is not a closed chain of lines and circular arcs.
    InvalidLoop,
    /// The offset consumes an arc or a segment: it is larger than the loop.
    OffsetTooLarge,
    /// The offset loop crosses itself: a neck closed up.
    OffsetSelfIntersects,
    /// A sharp reflex corner between an arc and its neighbour cannot be
    /// offset inside the analytic vocabulary.
    ReflexSharpCorner,
    /// Tangency, coincident carriers, or a chaining ambiguity the regularized
    /// Boolean refuses rather than guesses.
    BooleanUnsupported,
    /// The Boolean leaves no material at all.
    BooleanEmpty,
}

impl fmt::Display for CamQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLoop => "the loop is not a closed chain of lines and circular arcs",
            Self::OffsetTooLarge => "the offset is larger than the loop it is applied to",
            Self::OffsetSelfIntersects => "the offset loop crosses itself",
            Self::ReflexSharpCorner => {
                "a sharp reflex corner beside an arc cannot be offset exactly"
            }
            Self::BooleanUnsupported => {
                "the planar Boolean met a tangency or coincidence it refuses to guess at"
            }
            Self::BooleanEmpty => "the planar Boolean leaves no material",
        })
    }
}

impl std::error::Error for CamQueryError {}

impl From<LoopOffsetError> for CamQueryError {
    fn from(error: LoopOffsetError) -> Self {
        match error {
            LoopOffsetError::RadiusTooLarge => Self::OffsetTooLarge,
            LoopOffsetError::SelfIntersects => Self::OffsetSelfIntersects,
            LoopOffsetError::ReflexSharpCorner => Self::ReflexSharpCorner,
            LoopOffsetError::Degenerate => Self::InvalidLoop,
        }
    }
}

impl From<ProfileBooleanError> for CamQueryError {
    fn from(error: ProfileBooleanError) -> Self {
        match error {
            ProfileBooleanError::Unsupported => Self::BooleanUnsupported,
            ProfileBooleanError::EmptyResult => Self::BooleanEmpty,
        }
    }
}

impl NativeKernel {
    /// The `(r, z)` section of the snapshot's one solid, when it is a coaxial
    /// solid of revolution built from planes, cylinders, cones, spheres and
    /// tori; `None` for anything else.
    #[must_use]
    pub fn turned_section(snapshot: &Snapshot) -> Option<TurnedSection> {
        let section = extract_rz_section(&snapshot.topology).ok()?;
        Some(TurnedSection {
            center: protocol_point(section.center()),
            axis: protocol_vector(section.axis()),
            radial: protocol_vector(section.radial_u()),
            curves: curves_of(section.segments(), section.is_closed()),
            closed: section.is_closed(),
            sweep: section.sweep(),
        })
    }

    /// The profile of the snapshot's one solid when it is a prism whose
    /// extrusion direction is parallel to `axis`: planar anti-parallel caps
    /// and walls generated along the cap normal. `None` for anything else.
    #[must_use]
    pub fn prism_profile(snapshot: &Snapshot, axis: ProtocolVector3) -> Option<PrismProfile> {
        let precision = snapshot.precision.unwrap_or_default();
        let prism = extract_prism(&snapshot.topology, precision).ok()?;
        let frame = prism.frame();
        let wanted = Vector3::new(axis.x, axis.y, axis.z);
        let wanted_length = wanted.length();
        if !wanted_length.is_finite() || wanted_length <= f64::EPSILON {
            return None;
        }
        let normal_length = frame.normal.length();
        if normal_length <= f64::EPSILON {
            return None;
        }
        let cross = frame.normal.cross(wanted).length() / (normal_length * wanted_length);
        if cross > 1.0e-9 {
            return None;
        }
        let mut loops = prism.loops();
        let outer = loops.next()?;
        Some(PrismProfile {
            frame: PlanarFrame3::new(
                protocol_point(frame.origin),
                protocol_vector(frame.u),
                protocol_vector(frame.v),
            ),
            height: prism.height(),
            outer: loop_of(outer),
            holes: loops.map(loop_of).collect(),
        })
    }

    /// Whether a point lies inside the snapshot's material, by an exact
    /// parity ray cast against every face (shared by CAM, ADR 0057, and the
    /// simulation tab, ADR 0058).
    ///
    /// `None` where the answer cannot be given exactly: a point that is not
    /// finite, a face whose carrier the exact cast does not handle (cone,
    /// sphere, torus, ruled or B-spline), or every ray direction grazing a
    /// boundary. Points on the surface itself are among the latter.
    #[must_use]
    pub fn point_in_solid(snapshot: &Snapshot, point: ProtocolPoint3) -> Option<bool> {
        if !point.is_finite() {
            return None;
        }
        crate::analytic_boolean::point_in_solid(
            &snapshot.topology,
            Point3::new(point.x, point.y, point.z),
        )
    }

    /// The certified mitred offset of a closed loop of lines and circular
    /// arcs (ADR 0023's spine). A positive `distance` moves the loop towards
    /// its own interior, a negative one away from it, whichever way it
    /// winds; the result winds the way the source did. Sharp corners are
    /// mitred, and an offset that would consume a segment, invert an arc or
    /// cross itself is refused rather than guessed.
    ///
    /// The result holds one loop, or none when the offset leaves nothing.
    pub fn offset_loop(
        source: &PlanarLoop2,
        distance: f64,
    ) -> Result<Vec<PlanarLoop2>, CamQueryError> {
        if !distance.is_finite() || distance == 0.0 {
            return Err(CamQueryError::InvalidLoop);
        }
        let precision = PrecisionPolicy::default();
        let mut segments = segments_of(source, precision)?;
        let clockwise = signed_area(&segments) < 0.0;
        if clockwise {
            segments = reverse_loop(&segments);
        }
        let spine = match mitred_offset(&segments, distance, ReflexPolicy::MitreLines, precision) {
            Ok(spine) => spine,
            Err(LoopOffsetError::RadiusTooLarge | LoopOffsetError::SelfIntersects) => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        };
        let offset = if clockwise {
            reverse_loop(&spine.segments)
        } else {
            spine.segments
        };
        Ok(vec![loop_of(&offset)])
    }

    /// The exact regularized Boolean of two sets of disjoint planar regions
    /// of lines and circular arcs, as zero or more disjoint regions with
    /// their holes. Outer loops come back counter-clockwise and holes
    /// clockwise; the input may wind either way.
    pub fn profile_boolean(
        first: &[PlanarRegion2],
        second: &[PlanarRegion2],
        operation: BooleanOperation,
        precision: PrecisionPolicy,
    ) -> Result<Vec<PlanarRegion2>, CamQueryError> {
        let first = first
            .iter()
            .map(|region| region_of(region, precision))
            .collect::<Result<Vec<_>, _>>()?;
        let second = second
            .iter()
            .map(|region| region_of(region, precision))
            .collect::<Result<Vec<_>, _>>()?;
        let result = match profile_boolean_multi(&first, &second, operation, precision) {
            Ok(regions) => regions,
            Err(ProfileBooleanError::EmptyResult) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(result
            .into_iter()
            .map(|region| PlanarRegion2 {
                outer: loop_of(&region.outer),
                holes: region.holes.iter().map(|hole| loop_of(hole)).collect(),
            })
            .collect())
    }
}

fn region_of(
    region: &PlanarRegion2,
    precision: PrecisionPolicy,
) -> Result<ProfileRegion, CamQueryError> {
    Ok(ProfileRegion {
        outer: segments_of(&region.outer, precision)?,
        holes: region
            .holes
            .iter()
            .map(|hole| segments_of(hole, precision))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// A protocol loop as exact segments. A whole circle becomes its two
/// halves, seamed at azimuth `0` and `π` as ADR 0016 has it.
fn segments_of(
    source: &PlanarLoop2,
    precision: PrecisionPolicy,
) -> Result<Vec<Segment>, CamQueryError> {
    let minimum = precision.min_feature_size;
    let agreement = precision.linear_agreement;
    let mut segments = Vec::with_capacity(source.curves.len() + 1);
    for curve in &source.curves {
        match curve {
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            } => {
                if !radius.is_finite() || *radius <= minimum {
                    return Err(CamQueryError::InvalidLoop);
                }
                let center = Point2::new(center.x, center.y);
                let right = Point2::new(center.x + radius, center.y);
                let left = Point2::new(center.x - radius, center.y);
                let half = match direction {
                    ArcDirection::CounterClockwise => std::f64::consts::PI,
                    ArcDirection::Clockwise => -std::f64::consts::PI,
                };
                segments.push(Segment::Arc {
                    center,
                    start: right,
                    end: left,
                    radius: *radius,
                    start_angle: 0.0,
                    sweep: half,
                });
                segments.push(Segment::Arc {
                    center,
                    start: left,
                    end: right,
                    radius: *radius,
                    start_angle: std::f64::consts::PI,
                    sweep: half,
                });
            }
            other => {
                segments.push(
                    parse_curve(other, minimum, agreement)
                        .map_err(|_| CamQueryError::InvalidLoop)?,
                );
            }
        }
    }
    if segments.len() < 2 {
        return Err(CamQueryError::InvalidLoop);
    }
    let count = segments.len();
    for index in 0..count {
        let end = segments[index].end();
        let start = segments[(index + 1) % count].start();
        if (end.x - start.x).hypot(end.y - start.y) > minimum {
            return Err(CamQueryError::InvalidLoop);
        }
    }
    Ok(segments)
}

fn reverse_loop(segments: &[Segment]) -> Vec<Segment> {
    segments
        .iter()
        .rev()
        .map(|segment| match *segment {
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
            other => other.reversed(),
        })
        .collect()
}

fn signed_area(segments: &[Segment]) -> f64 {
    let mut area = 0.0;
    for segment in segments {
        let start = segment.start();
        let end = segment.end();
        area += start.x.mul_add(end.y, -(start.y * end.x)) / 2.0;
        if let Segment::Arc { radius, sweep, .. } = *segment {
            area += 0.5 * radius * radius * (sweep - sweep.sin());
        }
    }
    area
}

/// Segments as protocol curves, every junction emitted against one canonical
/// vertex so consecutive curves share their endpoint bit for bit.
fn loop_of(segments: &[Segment]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: curves_of(segments, true),
    }
}

/// A chain of segments as protocol curves. A `closed` chain's last curve
/// ends where its first begins; an open one (a section through the axis)
/// keeps its own last point.
fn curves_of(segments: &[Segment], closed: bool) -> Vec<PlanarCurve2> {
    let mut vertices = segments
        .iter()
        .map(|segment| segment.start())
        .collect::<Vec<_>>();
    if let Some(last) = segments.last() {
        vertices.push(if closed { vertices[0] } else { last.end() });
    }
    segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let from = vertices[index];
            let to = vertices[index + 1];
            match *segment {
                Segment::Arc { center, sweep, .. } => PlanarCurve2::CircularArc {
                    center: ProtocolPoint2::new(center.x, center.y),
                    start: ProtocolPoint2::new(from.x, from.y),
                    end: ProtocolPoint2::new(to.x, to.y),
                    direction: if sweep >= 0.0 {
                        ArcDirection::CounterClockwise
                    } else {
                        ArcDirection::Clockwise
                    },
                },
                // Sections and prism profiles carry lines and arcs only; the
                // section extractor and prism reader refuse everything else.
                _ => PlanarCurve2::Line {
                    start: ProtocolPoint2::new(from.x, from.y),
                    end: ProtocolPoint2::new(to.x, to.y),
                },
            }
        })
        .collect()
}

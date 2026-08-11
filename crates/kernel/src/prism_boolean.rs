//! Exact Booleans between prisms sharing an extrusion direction.
//!
//! When both operands are prisms along one direction and their slabs line up,
//! the 3D Boolean reduces *exactly* to the 2D regularized Boolean of their
//! profiles: union and intersection need the same slab, and difference needs
//! the tool to cover the target's full height. The result is itself a prism
//! over the combined profile, so it rebuilds through the already-certified
//! analytic extrusion path and inherits every one of its validation gates.
//!
//! This is deliberately the first inhabited corner of ADR 0025's
//! reconstruction plan rather than an approximation of the general case:
//! everything that reduces is computed in closed form, and everything that
//! does not — mismatched slabs, non-parallel axes, operands that are not
//! prisms at all — returns [`PrismBooleanError::DomainUnsupported`] so the
//! caller can fall through to the next strategy or refuse honestly.

use artificer_protocol::{
    ArcDirection, BooleanOperation, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy,
    Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::{
    Frame, Segment, build_analytic_extrusion, topology_loop_segments,
    validate_analytic_profile_extrusion,
};
use crate::profile_boolean::{
    Containment, ProfileBooleanError, ProfileRegion, imprinted_first_loops, profile_boolean,
    region_containment, welded,
};
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop, LoopKey,
    Orientation, ParameterRange, Plane, Point2, Point3, Record, Shell, ShellKey, Solid, Surface,
    Topology, Vector3, VertexKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrismBooleanError {
    /// The operands are not co-directional prisms with compatible slabs, or
    /// their profiles interact outside the regularized 2D domain.
    DomainUnsupported,
    /// The operation itself succeeded and produced no material.
    EmptyResult,
}

/// One operand seen as a prism: a bottom-anchored frame, a height, and the
/// profile loops in the frame's own plane coordinates.
struct Slab {
    frame: Frame,
    height: f64,
    outer: Vec<Segment>,
    holes: Vec<Vec<Segment>>,
}

/// Attempts the exact prism Boolean. `DomainUnsupported` means "not this
/// strategy", never "wrong answer".
pub(crate) fn build_prism_boolean(
    target: &Topology,
    tool: &Topology,
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Topology, PrismBooleanError> {
    // The shared direction is discovered, not assumed: any planar face of
    // either operand proposes its normal, and the first direction along which
    // *both* operands extract as prisms wins.
    let mut axes = candidate_axes(target);
    for axis in candidate_axes(tool) {
        if !axes
            .iter()
            .any(|known| known.cross(axis).length() <= 1.0e-9)
        {
            axes.push(axis);
        }
    }
    // Both senses of each axis matter: a pocket that opens downward is a
    // pocket that opens upward in the flipped frame, so the top-piercing
    // builder covers both by re-extraction rather than by a mirrored twin.
    // A strategy miss on one orientation is not a verdict, so the search
    // continues; an exact empty result is, so it returns.
    for axis in axes {
        for sense in [1.0, -1.0] {
            let oriented = axis * sense;
            let Some(target_slab) = extract_slab(target, oriented, precision) else {
                continue;
            };
            let Some(tool_slab) = extract_slab(tool, oriented, precision) else {
                continue;
            };
            match prism_boolean_along(&target_slab, &tool_slab, operation, precision) {
                Ok(topology) => return Ok(topology),
                Err(PrismBooleanError::EmptyResult) => {
                    return Err(PrismBooleanError::EmptyResult);
                }
                Err(PrismBooleanError::DomainUnsupported) => {}
            }
        }
    }
    Err(PrismBooleanError::DomainUnsupported)
}

/// Unit normals of the operand's planar faces, deduplicated by direction.
fn candidate_axes(topology: &Topology) -> Vec<Vector3> {
    let mut axes: Vec<Vector3> = Vec::new();
    for face in &topology.faces {
        if let Surface::Plane(plane) = face.value.surface {
            let length = plane.normal.length();
            if !length.is_finite() || length <= f64::EPSILON {
                continue;
            }
            let unit = plane.normal / length;
            if !axes
                .iter()
                .any(|known| known.cross(unit).length() <= 1.0e-9)
            {
                axes.push(unit);
            }
        }
    }
    axes
}

/// Reads the operand as a prism along `axis`: exactly two planar caps whose
/// normals lie along the axis (one each way), every other face swept parallel
/// to it. Returns the profile in the top cap's own plane coordinates with the
/// frame anchored at the bottom.
fn extract_slab(topology: &Topology, axis: Vector3, precision: PrecisionPolicy) -> Option<Slab> {
    if topology.solids.len() != 1 {
        return None;
    }
    let angular = precision.angular_agreement_radians.max(1.0e-12);
    let mut top = None;
    let mut bottom = None;
    for (index, face) in topology.faces.iter().enumerate() {
        let along = match face.value.surface {
            Surface::Plane(plane) => {
                let length = plane.normal.length();
                if !length.is_finite() || length <= f64::EPSILON {
                    return None;
                }
                let unit = plane.normal / length;
                if unit.cross(axis).length() <= angular {
                    if unit.dot(axis) > 0.0 {
                        if top.replace(index).is_some() {
                            return None;
                        }
                    } else if bottom.replace(index).is_some() {
                        return None;
                    }
                    continue;
                }
                plane.normal.dot(axis).abs() <= angular * length
            }
            Surface::Cylinder(cylinder) => cylinder.axis.cross(axis).length() <= angular,
            Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => false,
        };
        if !along {
            return None;
        }
    }
    let top = &topology.faces[top?].value;
    let bottom = &topology.faces[bottom?].value;
    let top_plane = top.surface.as_plane()?;
    let bottom_plane = bottom.surface.as_plane()?;
    let height = (top_plane.origin - bottom_plane.origin).dot(axis);
    if !height.is_finite() || height <= precision.min_feature_size {
        return None;
    }
    let outer = topology_loop_segments(topology, top.outer_loop)?;
    let holes = top
        .inner_loops
        .iter()
        .map(|loop_key| topology_loop_segments(topology, *loop_key))
        .collect::<Option<Vec<_>>>()?;
    Some(Slab {
        frame: Frame {
            origin: top_plane.origin + axis * -height,
            u: top_plane.u,
            v: top_plane.v,
            normal: axis,
        },
        height,
        outer,
        holes,
    })
}

fn prism_boolean_along(
    target: &Slab,
    tool: &Slab,
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Topology, PrismBooleanError> {
    let agreement = precision.linear_agreement.max(1.0e-12)
        * (1.0 + target.height.abs().max(tool.height.abs()));
    let offset = (tool.frame.origin - target.frame.origin).dot(target.frame.normal);

    // The tool's profile, re-read in the target's plane coordinates.
    let into_target = |point: Point2| {
        let world = tool.frame.point(point, 0.0);
        let offset = world - target.frame.origin;
        Point2::new(offset.dot(target.frame.u), offset.dot(target.frame.v))
    };
    let map_segment = |segment: &Segment| match *segment {
        Segment::Line { start, end } => Segment::Line {
            start: into_target(start),
            end: into_target(end),
        },
        Segment::Arc {
            center,
            start,
            end,
            radius,
            sweep,
            ..
        } => {
            let center = into_target(center);
            let start = into_target(start);
            Segment::Arc {
                center,
                start,
                end: into_target(end),
                radius,
                start_angle: (start.y - center.y).atan2(start.x - center.x),
                sweep,
            }
        }
    };
    let map_loop = |segments: &[Segment]| segments.iter().map(map_segment).collect::<Vec<_>>();

    let target_region = ProfileRegion {
        outer: target.outer.clone(),
        holes: target.holes.clone(),
    };
    let tool_region = ProfileRegion {
        outer: map_loop(&tool.outer),
        holes: tool.holes.iter().map(|hole| map_loop(hole)).collect(),
    };

    // Slab compatibility. A union must keep one slab (equal ranges); an
    // intersection is a prism over the slab *overlap* whatever the ranges;
    // a difference either removes material through the target's full height
    // or, when the tool stops inside and pierces exactly the top cap, builds
    // a blind pocket through the stacked path.
    let mut slab_bottom = 0.0;
    let mut slab_height = target.height;
    let compatible = match operation {
        BooleanOperation::Union => {
            offset.abs() <= agreement && (target.height - tool.height).abs() <= agreement
        }
        BooleanOperation::Intersection => {
            slab_bottom = offset.max(0.0);
            let top = (offset + tool.height).min(target.height);
            slab_height = top - slab_bottom;
            if slab_height <= precision.min_feature_size {
                return Err(PrismBooleanError::EmptyResult);
            }
            true
        }
        BooleanOperation::Difference => {
            let covers = offset <= agreement && offset + tool.height >= target.height - agreement;
            if !covers {
                let pierces_top = offset + tool.height >= target.height - agreement;
                let floor_inside = offset >= precision.min_feature_size
                    && target.height - offset >= precision.min_feature_size;
                let interior = floor_inside
                    && offset + tool.height <= target.height - precision.min_feature_size;
                if !pierces_top && !interior {
                    // Piercing the bottom only is the flipped sense's job;
                    // anything else has no stacked reduction.
                    return Err(PrismBooleanError::DomainUnsupported);
                }
                if interior {
                    // A closed cavity must be strictly inside laterally: a
                    // tool crossing the wall at interior height would open a
                    // side pocket, which has no stacked reduction yet.
                    if region_containment(&tool_region, &target_region, precision)
                        .map_err(|_| PrismBooleanError::DomainUnsupported)?
                        != Containment::StrictlyInside
                    {
                        return Err(PrismBooleanError::DomainUnsupported);
                    }
                    return build_interior_void(
                        target,
                        &tool_region,
                        offset,
                        tool.height,
                        precision,
                    );
                }
                // A blind pocket of any lateral shape: the floor is the 2D
                // intersection, the material above it the 2D difference, and
                // the lower layer the target imprinted with the crossings.
                let floor_regions = match profile_boolean(
                    &target_region,
                    &tool_region,
                    BooleanOperation::Intersection,
                    precision,
                ) {
                    Ok(regions) => regions,
                    Err(ProfileBooleanError::EmptyResult) => {
                        // The tool never touches the profile: the difference
                        // is the identity, republished as the target prism.
                        return build_stacked_pocket(
                            target,
                            &[target.outer.clone()]
                                .into_iter()
                                .chain(target.holes.iter().cloned())
                                .map(|segments| welded(&segments, precision))
                                .collect::<Result<Vec<_>, _>>()
                                .map_err(|_| PrismBooleanError::DomainUnsupported)?,
                            &[],
                            &[],
                            target.height,
                            precision,
                        );
                    }
                    Err(ProfileBooleanError::Unsupported) => {
                        return Err(PrismBooleanError::DomainUnsupported);
                    }
                };
                let upper_regions = match profile_boolean(
                    &target_region,
                    &tool_region,
                    BooleanOperation::Difference,
                    precision,
                ) {
                    Ok(regions) => regions,
                    Err(ProfileBooleanError::EmptyResult) => Vec::new(),
                    Err(ProfileBooleanError::Unsupported) => {
                        return Err(PrismBooleanError::DomainUnsupported);
                    }
                };
                let lower_loops = imprinted_first_loops(&target_region, &tool_region, precision)
                    .map_err(|_| PrismBooleanError::DomainUnsupported)?;
                return build_stacked_pocket(
                    target,
                    &lower_loops,
                    &upper_regions,
                    &floor_regions,
                    offset,
                    precision,
                );
            }
            true
        }
    };
    if !compatible {
        return Err(PrismBooleanError::DomainUnsupported);
    }

    let regions =
        profile_boolean(&target_region, &tool_region, operation, precision).map_err(|error| {
            match error {
                ProfileBooleanError::Unsupported => PrismBooleanError::DomainUnsupported,
                ProfileBooleanError::EmptyResult => PrismBooleanError::EmptyResult,
            }
        })?;

    // Rebuild through the certified extrusion path, inheriting its region
    // disjointness, nesting, and minimum-feature gates.
    let profile = PlanarProfile2 {
        regions: regions
            .iter()
            .map(|region| PlanarRegion2 {
                outer: protocol_loop(&region.outer),
                holes: region
                    .holes
                    .iter()
                    .map(|hole| protocol_loop(hole))
                    .collect(),
            })
            .collect(),
    };
    let base = target.frame.origin + target.frame.normal * slab_bottom;
    let frame = PlanarFrame3::new(
        ProtocolPoint3::new(base.x, base.y, base.z),
        ProtocolVector3::new(target.frame.u.x, target.frame.u.y, target.frame.u.z),
        ProtocolVector3::new(target.frame.v.x, target.frame.v.y, target.frame.v.z),
    );
    let validated = validate_analytic_profile_extrusion(frame, &profile, slab_height, precision)
        .map_err(|_| PrismBooleanError::DomainUnsupported)?;
    Ok(build_analytic_extrusion(&validated))
}

fn protocol_loop(segments: &[Segment]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: segments
            .iter()
            .map(|segment| match *segment {
                Segment::Line { start, end } => PlanarCurve2::Line {
                    start: ProtocolPoint2::new(start.x, start.y),
                    end: ProtocolPoint2::new(end.x, end.y),
                },
                Segment::Arc {
                    center,
                    start,
                    end,
                    sweep,
                    ..
                } => PlanarCurve2::CircularArc {
                    center: ProtocolPoint2::new(center.x, center.y),
                    start: ProtocolPoint2::new(start.x, start.y),
                    end: ProtocolPoint2::new(end.x, end.y),
                    direction: if sweep >= 0.0 {
                        ArcDirection::CounterClockwise
                    } else {
                        ArcDirection::Clockwise
                    },
                },
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Blind pockets: the stacked builder
// ---------------------------------------------------------------------------

/// Builds the difference of a blind pocket: a tool prism strictly inside the
/// target's profile that pierces exactly the top cap, stopping at a floor
/// height inside the slab.
///
/// The result is two certified extrusion layers glued at the floor plane:
/// the full profile below, the profile with the tool as an extra hole above.
/// Gluing is entity surgery, not geometry: the layers' shared rim vertices
/// and edges are welded into single records, both interface caps are
/// removed, and one new planar floor face takes their place over the tool
/// region, reusing the pocket wall's existing bottom rim edges. Every
/// invariant the surgery must preserve — edge use counts and senses, pcurve
/// loci, Euler characteristic — is then checked by the validator on the
/// merged result exactly as for any other candidate.
/// Builds a blind pocket of any lateral shape: strictly interior, crossing
/// the target's boundary, or from a holed tool that leaves islands.
///
/// The lower layer is the target profile *imprinted* — split at every
/// crossing with the tool, so its rim edges align piece for piece with the
/// upper layer's. The upper layer is the 2D difference (possibly several
/// regions: a holed tool's island is a pillar standing on the floor), and
/// the floors are the 2D intersection (possibly several regions, each with
/// holes: an annular tool has an annular floor). All three derive from the
/// same deterministic imprint, so their shared vertices are the same floats.
fn build_stacked_pocket(
    target: &Slab,
    lower_loops: &[Vec<Segment>],
    upper_regions: &[ProfileRegion],
    floor_regions: &[ProfileRegion],
    floor_height: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, PrismBooleanError> {
    let (Some(lower_outer), lower_holes) = (lower_loops.first(), &lower_loops[1..]) else {
        return Err(PrismBooleanError::DomainUnsupported);
    };
    let lower_frame = protocol_frame(target.frame.origin, target.frame);
    let lower_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: protocol_loop(lower_outer),
            holes: lower_holes.iter().map(|hole| protocol_loop(hole)).collect(),
        }],
    };
    let lower =
        validate_analytic_profile_extrusion(lower_frame, &lower_profile, floor_height, precision)
            .map_err(|_| PrismBooleanError::DomainUnsupported)?;
    let lower = build_analytic_extrusion(&lower);

    if upper_regions.is_empty() {
        // The tool removes everything above the floor: the result is simply
        // the lower prism, whose own top cap is the floor.
        return Ok(lower);
    }

    let upper_base = target.frame.origin + target.frame.normal * floor_height;
    let upper_profile = PlanarProfile2 {
        regions: upper_regions
            .iter()
            .map(|region| PlanarRegion2 {
                outer: protocol_loop(&region.outer),
                holes: region
                    .holes
                    .iter()
                    .map(|hole| protocol_loop(hole))
                    .collect(),
            })
            .collect(),
    };
    let upper = validate_analytic_profile_extrusion(
        protocol_frame(upper_base, target.frame),
        &upper_profile,
        target.height - floor_height,
        precision,
    )
    .map_err(|_| PrismBooleanError::DomainUnsupported)?;
    let upper = build_analytic_extrusion(&upper);

    glue_layers(
        &lower,
        &upper,
        target,
        floor_regions,
        floor_height,
        precision,
    )
}

/// Builds the difference of a strictly interior tool: the target prism with
/// a closed cavity inside it.
///
/// The cavity's boundary is the tool prism's own boundary with its material
/// side flipped — every loop reversed, every coedge's sense and pcurve
/// direction inverted — carried as an inner shell of the solid. With the
/// cavity faces oriented away from the material, every flux-based measure
/// subtracts the void without a special case, and the validator checks the
/// reversed shell with exactly the families it applies to any other.
fn build_interior_void(
    target: &Slab,
    tool_region: &ProfileRegion,
    floor_height: f64,
    tool_height: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, PrismBooleanError> {
    let weld_all = |loops: &[Vec<Segment>]| -> Result<Vec<Vec<Segment>>, PrismBooleanError> {
        loops
            .iter()
            .map(|segments| {
                welded(segments, precision).map_err(|_| PrismBooleanError::DomainUnsupported)
            })
            .collect()
    };
    let outer =
        welded(&target.outer, precision).map_err(|_| PrismBooleanError::DomainUnsupported)?;
    let holes = weld_all(&target.holes)?;
    let tool_outer =
        welded(&tool_region.outer, precision).map_err(|_| PrismBooleanError::DomainUnsupported)?;
    let tool_holes = weld_all(&tool_region.holes)?;

    let shell_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: protocol_loop(&outer),
            holes: holes.iter().map(|hole| protocol_loop(hole)).collect(),
        }],
    };
    let shell_frame = protocol_frame(target.frame.origin, target.frame);
    let shell =
        validate_analytic_profile_extrusion(shell_frame, &shell_profile, target.height, precision)
            .map_err(|_| PrismBooleanError::DomainUnsupported)?;
    let shell = build_analytic_extrusion(&shell);

    let cavity_base = target.frame.origin + target.frame.normal * floor_height;
    // A holed tool leaves its island column attached above and below, so
    // the cavity is an annular tube: its closed boundary shell carries the
    // hole walls, and the shell's genus rises accordingly.
    let cavity_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: protocol_loop(&tool_outer),
            holes: tool_holes.iter().map(|hole| protocol_loop(hole)).collect(),
        }],
    };
    let cavity = validate_analytic_profile_extrusion(
        protocol_frame(cavity_base, target.frame),
        &cavity_profile,
        tool_height,
        precision,
    )
    .map_err(|_| PrismBooleanError::DomainUnsupported)?;
    let mut cavity = build_analytic_extrusion(&cavity);
    reverse_shell_orientation(&mut cavity)?;

    // Merge: the two shells share nothing, so this is pure concatenation
    // with offsets, one solid owning both.
    let mut merged = shell.clone();
    let vertex_offset = merged.vertices.len();
    let edge_offset = merged.edges.len();
    let coedge_offset = merged.coedges.len();
    let loop_offset = merged.loops.len();
    let face_offset = merged.faces.len();
    let shell_offset = merged.shells.len();
    for vertex in &cavity.vertices {
        merged.vertices.push(vertex.clone());
    }
    for edge in &cavity.edges {
        let mut record = edge.clone();
        record.value.vertices = record
            .value
            .vertices
            .map(|vertex| VertexKey(vertex.0 + vertex_offset));
        merged.edges.push(record);
    }
    for coedge in &cavity.coedges {
        let mut record = coedge.clone();
        record.value.edge = EdgeKey(record.value.edge.0 + edge_offset);
        merged.coedges.push(record);
    }
    for loop_record in &cavity.loops {
        let mut record = loop_record.clone();
        record.value.coedges = record
            .value
            .coedges
            .iter()
            .map(|key| CoedgeKey(key.0 + coedge_offset))
            .collect();
        merged.loops.push(record);
    }
    for face in &cavity.faces {
        let mut record = face.clone();
        record.value.outer_loop = LoopKey(record.value.outer_loop.0 + loop_offset);
        record.value.inner_loops = record
            .value
            .inner_loops
            .iter()
            .map(|key| LoopKey(key.0 + loop_offset))
            .collect();
        merged.faces.push(record);
    }
    for cavity_shell in &cavity.shells {
        let mut record = cavity_shell.clone();
        record.value.faces = record
            .value
            .faces
            .iter()
            .map(|key| FaceKey(key.0 + face_offset))
            .collect();
        merged.shells.push(record);
    }
    if cavity.solids.len() != 1 || merged.solids.len() != 1 {
        return Err(PrismBooleanError::DomainUnsupported);
    }
    merged.solids[0].value.inner_shells = vec![ShellKey(shell_offset)];
    Ok(compact(merged))
}

/// Flips which side of a closed shell is material.
///
/// The validator's convention is that every face's outer loop winds
/// positively in its own parameter frame — reverse the surface, never the
/// loop. So the flip mirrors each face's surface (a plane swaps its u and v,
/// a cylinder negates its angular sign), maps every pcurve through the same
/// in-plane mirror, and reverses the traversal so the loop stays positive in
/// the mirrored frame. Edges and vertices are untouched.
fn reverse_shell_orientation(topology: &mut Topology) -> Result<(), PrismBooleanError> {
    for face in &mut topology.faces {
        // The in-plane mirror matching the surface flip, as a linear map.
        let mirror: fn(Point2) -> Point2 = match &mut face.value.surface {
            Surface::Plane(plane) => {
                *plane = Plane::new(plane.origin, plane.v, plane.u);
                |point: Point2| Point2::new(point.y, point.x)
            }
            Surface::Cylinder(cylinder) => {
                cylinder.angular_sign = -cylinder.angular_sign;
                |point: Point2| Point2::new(-point.x, point.y)
            }
            Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => {
                return Err(PrismBooleanError::DomainUnsupported);
            }
        };
        for loop_key in face.value.loops().collect::<Vec<_>>() {
            let loop_record = &mut topology.loops[loop_key.0];
            loop_record.value.coedges.reverse();
            for coedge_key in loop_record.value.coedges.clone() {
                let coedge = &mut topology.coedges[coedge_key.0].value;
                coedge.orientation = coedge.orientation.reversed();
                let range = coedge.parameter_range;
                match coedge.pcurve {
                    Curve2::Line { .. } => {
                        let start = mirror(coedge.pcurve.evaluate(range.start));
                        let end = mirror(coedge.pcurve.evaluate(range.end));
                        let (pcurve, parameter_range) = Curve2::line_segment([end, start]);
                        coedge.pcurve = pcurve;
                        coedge.parameter_range = parameter_range;
                    }
                    Curve2::Circle {
                        center,
                        u,
                        v,
                        radius,
                    } => {
                        let map_vector = |vector: crate::topology::Vector2| {
                            let mapped = mirror(Point2::new(vector.x, vector.y));
                            crate::topology::Vector2::new(mapped.x, mapped.y)
                        };
                        coedge.pcurve = Curve2::Circle {
                            center: mirror(center),
                            u: map_vector(u),
                            v: map_vector(v),
                            radius,
                        };
                        coedge.parameter_range = ParameterRange::new(range.end, range.start);
                    }
                }
            }
        }
    }
    Ok(())
}

fn protocol_frame(origin: Point3, frame: Frame) -> PlanarFrame3 {
    PlanarFrame3::new(
        ProtocolPoint3::new(origin.x, origin.y, origin.z),
        ProtocolVector3::new(frame.u.x, frame.u.y, frame.u.z),
        ProtocolVector3::new(frame.v.x, frame.v.y, frame.v.z),
    )
}

/// Welds two stacked layer topologies into one solid with a floor face.
fn glue_layers(
    lower: &Topology,
    upper: &Topology,
    target: &Slab,
    floor_regions: &[ProfileRegion],
    floor_height: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, PrismBooleanError> {
    let scale = target
        .outer
        .iter()
        .map(|segment| segment.start().x.abs().max(segment.start().y.abs()))
        .fold(target.height.abs().max(1.0), f64::max);
    let weld = precision.linear_agreement.max(1.0e-12) * scale * 8.0;

    // The interface caps, soon to be removed: the lower layer's single top
    // cap, and every bottom cap of the upper layer (one per region — a holed
    // tool's island region brings its own).
    let lower_cap = single_role_face(lower, FaceRole::ExtrusionTop)?;
    let upper_caps: Vec<usize> = upper
        .faces
        .iter()
        .enumerate()
        .filter(|(_, face)| face.value.role == FaceRole::ExtrusionBottom)
        .map(|(index, _)| index)
        .collect();
    if upper_caps.is_empty() {
        return Err(PrismBooleanError::DomainUnsupported);
    }

    // Start the merged arrays from the lower layer verbatim.
    let mut merged = lower.clone();

    // Vertex weld: every upper vertex at the interface that coincides with a
    // lower vertex adopts the lower record; everything else is appended.
    let mut vertex_map: Vec<VertexKey> = Vec::with_capacity(upper.vertices.len());
    for vertex in &upper.vertices {
        let point = vertex.value.point;
        let existing = lower
            .vertices
            .iter()
            .position(|candidate| (candidate.value.point - point).length() <= weld);
        if let Some(index) = existing {
            vertex_map.push(VertexKey(index));
        } else {
            let key = VertexKey(merged.vertices.len());
            merged.vertices.push(vertex.clone());
            vertex_map.push(key);
        }
    }

    // Edge weld: an upper edge whose endpoints both mapped onto lower
    // vertices and whose carrier matches an existing lower edge adopts that
    // record, flipping the sense when the vertex order swapped.
    let mut edge_map: Vec<(EdgeKey, bool)> = Vec::with_capacity(upper.edges.len());
    for edge in &upper.edges {
        let mapped = [
            vertex_map[edge.value.vertices[0].0],
            vertex_map[edge.value.vertices[1].0],
        ];
        let welded = (mapped[0].0 < lower.vertices.len() && mapped[1].0 < lower.vertices.len())
            .then(|| {
                lower.edges.iter().position(|candidate| {
                    let vertices = candidate.value.vertices;
                    let aligned = vertices == mapped;
                    let swapped = vertices == [mapped[1], mapped[0]];
                    (aligned || swapped)
                        && same_carrier(&candidate.value.curve, &edge.value.curve, weld)
                })
            })
            .flatten();
        if let Some(index) = welded {
            let swapped = lower.edges[index].value.vertices != mapped;
            edge_map.push((EdgeKey(index), swapped));
        } else {
            let key = EdgeKey(merged.edges.len());
            let mut record = edge.clone();
            record.value.vertices = mapped;
            merged.edges.push(record);
            edge_map.push((key, false));
        }
    }

    // Append the upper coedges, loops, and faces, dropping both caps.
    let coedge_offset = merged.coedges.len();
    for coedge in &upper.coedges {
        let (edge, swapped) = edge_map[coedge.value.edge.0];
        let mut record = coedge.clone();
        record.value.edge = edge;
        if swapped {
            record.value.orientation = record.value.orientation.reversed();
        }
        merged.coedges.push(record);
    }
    let loop_offset = merged.loops.len();
    for loop_record in &upper.loops {
        let mut record = loop_record.clone();
        record.value.coedges = record
            .value
            .coedges
            .iter()
            .map(|key| CoedgeKey(key.0 + coedge_offset))
            .collect();
        merged.loops.push(record);
    }
    let mut faces: Vec<FaceKey> = (0..lower.faces.len())
        .filter(|index| *index != lower_cap)
        .map(FaceKey)
        .collect();
    for (index, face) in upper.faces.iter().enumerate() {
        if upper_caps.contains(&index) {
            continue;
        }
        let key = FaceKey(merged.faces.len());
        let mut record = face.clone();
        record.value.outer_loop = LoopKey(record.value.outer_loop.0 + loop_offset);
        record.value.inner_loops = record
            .value
            .inner_loops
            .iter()
            .map(|loop_key| LoopKey(loop_key.0 + loop_offset))
            .collect();
        merged.faces.push(record);
        faces.push(key);
    }

    // The floors: one planar face per intersection region, each boundary
    // loop reusing rim edges already present in the merged topology. A
    // boundary-crossing pocket borrows the lower layer's split wall-top
    // edges along the target boundary and the upper layer's pocket-wall
    // bottoms along the tool boundary; an annular floor carries the tool's
    // hole as an inner loop whose edges are the island pillar's bottom rim.
    let floor_base = target.frame.origin + target.frame.normal * floor_height;
    let mut next_id = merged
        .vertices
        .iter()
        .map(|record| record.id.get())
        .chain(merged.edges.iter().map(|record| record.id.get()))
        .chain(merged.coedges.iter().map(|record| record.id.get()))
        .chain(merged.loops.iter().map(|record| record.id.get()))
        .chain(merged.faces.iter().map(|record| record.id.get()))
        .chain(merged.shells.iter().map(|record| record.id.get()))
        .chain(merged.solids.iter().map(|record| record.id.get()))
        .max()
        .unwrap_or(0)
        + 1;
    let floor_loop_from = |merged: &mut Topology,
                           next_id: &mut u64,
                           segments: &[Segment]|
     -> Result<LoopKey, PrismBooleanError> {
        let mut floor_coedges = Vec::with_capacity(segments.len());
        for segment in segments {
            let start_world = target.frame.point(segment.start(), floor_height);
            let end_world = target.frame.point(segment.end(), floor_height);
            // Endpoints alone cannot identify the edge: the two semicircles
            // of a full circle share both seam vertices. The midpoint can.
            let middle_world = target.frame.point(segment_midpoint(*segment), floor_height);
            let found = merged.edges.iter().enumerate().find(|(_, edge)| {
                let first = merged.vertices[edge.value.vertices[0].0].value.point;
                let second = merged.vertices[edge.value.vertices[1].0].value.point;
                let aligned =
                    (first - start_world).length() <= weld && (second - end_world).length() <= weld;
                let swapped =
                    (first - end_world).length() <= weld && (second - start_world).length() <= weld;
                if !aligned && !swapped {
                    return false;
                }
                let range = edge.value.parameter_range;
                let edge_middle = edge.value.curve.evaluate((range.start + range.end) / 2.0);
                (edge_middle - middle_world).length() <= weld
            });
            let Some((edge_index, edge)) = found else {
                return Err(PrismBooleanError::DomainUnsupported);
            };
            let first = merged.vertices[edge.value.vertices[0].0].value.point;
            let orientation = if (first - start_world).length() <= weld {
                Orientation::Forward
            } else {
                Orientation::Reverse
            };
            let (pcurve, parameter_range) = match *segment {
                Segment::Line { start, end } => Curve2::line_segment([start, end]),
                Segment::Arc {
                    center,
                    radius,
                    start_angle,
                    sweep,
                    ..
                } => (
                    Curve2::Circle {
                        center,
                        u: crate::topology::Vector2::new(1.0, 0.0),
                        v: crate::topology::Vector2::new(0.0, 1.0),
                        radius,
                    },
                    ParameterRange::new(start_angle, start_angle + sweep),
                ),
            };
            let coedge_key = CoedgeKey(merged.coedges.len());
            merged.coedges.push(Record {
                id: EntityId::from_raw(*next_id),
                value: Coedge {
                    edge: EdgeKey(edge_index),
                    orientation,
                    pcurve,
                    parameter_range,
                },
            });
            *next_id += 1;
            floor_coedges.push(coedge_key);
        }
        let floor_loop = LoopKey(merged.loops.len());
        merged.loops.push(Record {
            id: EntityId::from_raw(*next_id),
            value: Loop {
                coedges: floor_coedges,
            },
        });
        *next_id += 1;
        Ok(floor_loop)
    };
    for region in floor_regions {
        let outer_loop = floor_loop_from(&mut merged, &mut next_id, &region.outer)?;
        let inner_loops = region
            .holes
            .iter()
            .map(|hole| floor_loop_from(&mut merged, &mut next_id, hole))
            .collect::<Result<Vec<_>, _>>()?;
        let floor_face = FaceKey(merged.faces.len());
        merged.faces.push(Record {
            id: EntityId::from_raw(next_id),
            value: Face {
                surface: Surface::Plane(Plane::new(floor_base, target.frame.u, target.frame.v)),
                outer_loop,
                inner_loops,
                role: FaceRole::FeatureEnd,
            },
        });
        next_id += 1;
        faces.push(floor_face);
    }

    // One shell, one solid, and a compaction pass that drops the removed
    // caps' records and every orphan they leave behind.
    merged.shells = vec![Record {
        id: EntityId::from_raw(next_id),
        value: Shell { faces },
    }];
    next_id += 1;
    merged.solids = vec![Record {
        id: EntityId::from_raw(next_id),
        value: Solid {
            outer_shell: ShellKey(0),
            inner_shells: Vec::new(),
        },
    }];
    Ok(compact(merged))
}

fn single_role_face(topology: &Topology, role: FaceRole) -> Result<usize, PrismBooleanError> {
    let mut found = None;
    for (index, face) in topology.faces.iter().enumerate() {
        if face.value.role == role {
            if found.is_some() {
                return Err(PrismBooleanError::DomainUnsupported);
            }
            found = Some(index);
        }
    }
    found.ok_or(PrismBooleanError::DomainUnsupported)
}

/// Whether two 3D curves describe the same carrier within the weld distance.
fn same_carrier(first: &Curve3, second: &Curve3, weld: f64) -> bool {
    match (first, second) {
        (Curve3::Line { .. }, Curve3::Line { .. }) => true,
        (
            Curve3::Circle {
                center: first_center,
                u: first_u,
                v: first_v,
                radius: first_radius,
            },
            Curve3::Circle {
                center: second_center,
                u: second_u,
                v: second_v,
                radius: second_radius,
            },
        ) => {
            (*first_center - *second_center).length() <= weld
                && (*first_u - *second_u).length() <= weld
                && (*first_v - *second_v).length() <= weld
                && (first_radius - second_radius).abs() <= weld
        }
        _ => false,
    }
}

/// The 2D midpoint of a profile segment, in its own plane coordinates.
fn segment_midpoint(segment: Segment) -> Point2 {
    match segment {
        Segment::Line { start, end } => {
            Point2::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0)
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let angle = sweep.mul_add(0.5, start_angle);
            Point2::new(
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            )
        }
    }
}

/// Rebuilds the topology keeping only entities reachable from its solids,
/// renumbering keys and identifiers densely.
fn compact(source: Topology) -> Topology {
    let mut used_faces = vec![false; source.faces.len()];
    for shell in &source.shells {
        for face in &shell.value.faces {
            used_faces[face.0] = true;
        }
    }
    let mut used_loops = vec![false; source.loops.len()];
    for (index, face) in source.faces.iter().enumerate() {
        if used_faces[index] {
            for loop_key in face.value.loops() {
                used_loops[loop_key.0] = true;
            }
        }
    }
    let mut used_coedges = vec![false; source.coedges.len()];
    for (index, loop_record) in source.loops.iter().enumerate() {
        if used_loops[index] {
            for coedge in &loop_record.value.coedges {
                used_coedges[coedge.0] = true;
            }
        }
    }
    let mut used_edges = vec![false; source.edges.len()];
    for (index, coedge) in source.coedges.iter().enumerate() {
        if used_coedges[index] {
            used_edges[coedge.value.edge.0] = true;
        }
    }
    let mut used_vertices = vec![false; source.vertices.len()];
    for (index, edge) in source.edges.iter().enumerate() {
        if used_edges[index] {
            for vertex in edge.value.vertices {
                used_vertices[vertex.0] = true;
            }
        }
    }

    let remap = |used: &[bool]| -> Vec<usize> {
        let mut next = 0;
        used.iter()
            .map(|keep| {
                if *keep {
                    next += 1;
                    next - 1
                } else {
                    usize::MAX
                }
            })
            .collect()
    };
    let vertex_remap = remap(&used_vertices);
    let edge_remap = remap(&used_edges);
    let coedge_remap = remap(&used_coedges);
    let loop_remap = remap(&used_loops);
    let face_remap = remap(&used_faces);

    let mut result = Topology::default();
    let mut next_id = 1_u64;
    let mut fresh = |record_id: &mut EntityId| {
        *record_id = EntityId::from_raw(next_id);
        next_id += 1;
    };
    for (index, vertex) in source.vertices.iter().enumerate() {
        if used_vertices[index] {
            let mut record = vertex.clone();
            fresh(&mut record.id);
            result.vertices.push(record);
        }
    }
    for (index, edge) in source.edges.iter().enumerate() {
        if used_edges[index] {
            let mut record = edge.clone();
            record.value.vertices = record
                .value
                .vertices
                .map(|vertex| VertexKey(vertex_remap[vertex.0]));
            fresh(&mut record.id);
            result.edges.push(record);
        }
    }
    for (index, coedge) in source.coedges.iter().enumerate() {
        if used_coedges[index] {
            let mut record = coedge.clone();
            record.value.edge = EdgeKey(edge_remap[record.value.edge.0]);
            fresh(&mut record.id);
            result.coedges.push(record);
        }
    }
    for (index, loop_record) in source.loops.iter().enumerate() {
        if used_loops[index] {
            let mut record = loop_record.clone();
            record.value.coedges = record
                .value
                .coedges
                .iter()
                .map(|coedge| CoedgeKey(coedge_remap[coedge.0]))
                .collect();
            fresh(&mut record.id);
            result.loops.push(record);
        }
    }
    for (index, face) in source.faces.iter().enumerate() {
        if used_faces[index] {
            let mut record = face.clone();
            record.value.outer_loop = LoopKey(loop_remap[record.value.outer_loop.0]);
            record.value.inner_loops = record
                .value
                .inner_loops
                .iter()
                .map(|loop_key| LoopKey(loop_remap[loop_key.0]))
                .collect();
            fresh(&mut record.id);
            result.faces.push(record);
        }
    }
    for shell in &source.shells {
        let mut record = shell.clone();
        record.value.faces = record
            .value
            .faces
            .iter()
            .map(|face| FaceKey(face_remap[face.0]))
            .collect();
        fresh(&mut record.id);
        result.shells.push(record);
    }
    for solid in &source.solids {
        let mut record = solid.clone();
        fresh(&mut record.id);
        result.solids.push(record);
    }
    result
}

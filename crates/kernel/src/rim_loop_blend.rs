//! Exact fillets and chamfers around a whole cap rim of a prism.
//!
//! Sweeping the chamfer around a rim shrinks the cap to the *mitred* inward
//! offset of the profile — the spine — and replaces the top of every wall with
//! a slant face running from the profile line at `h − d` to the spine line at
//! `h`. Those two lines are parallel, so each slant is exactly planar, and two
//! adjacent slants meet along the mitre edge that rises from the surviving
//! sharp wall corner to the cap's mitre corner. No corner face is needed and
//! no new surface class enters: the whole result stays in the plane vocabulary
//! and therefore keeps exact polyhedral measures.
//!
//! A fillet sweeps a ball of radius `f` along the same spine. Over a straight
//! profile segment the ball traces a quarter cylinder, over a convex arc a
//! quarter torus about the same centre, and where two segments meet
//! tangentially the neighbouring bands already share one seam arc. At a sharp
//! convex corner the ball pivots about the mitre point, tracing a sphere
//! patch. That patch
//! is tangent to the cap, so it meets the cap plane at a single pole point
//! rather than along an arc: its lower boundary is an equator arc, and the
//! face between that equator and the surviving sharp wall corner is a flat
//! ledge. Its two meridians are exactly the adjacent bands' end arcs, so the
//! patch closes as a three-sided face and no pole-edge vocabulary is needed.

use artificer_protocol::{EdgeFinishKind, EntityKind, EntityRef, PrecisionPolicy, SnapshotId};

use crate::analytic_extrusion::Segment;
use crate::loop_offset::{LoopOffsetError, SpineLoop, mitred_inward_offset};
use crate::prism_edge_finish::{PrismProfile, extract_prism};
use crate::topology::{
    Coedge, CoedgeKey, Cone, Curve2, Curve3, Cylinder, Edge, EdgeKey, EntityId, Face, FaceKey,
    FaceRole, Loop, LoopKey, Orientation, ParameterRange, Plane, Point2, Point3, Record, Shell,
    ShellKey, Solid, Sphere, Surface, Topology, Torus, Vector2, Vector3, Vertex, VertexKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RimLoopBlendError {
    TargetInvalid,
    DomainUnsupported,
    DistanceInvalid,
    /// A sharp concave corner: the two blend faces would meet in a curve
    /// outside the analytic vocabulary.
    ReflexCorner,
}

/// Chamfers the complete outer rim loop of one cap.
pub(crate) fn build_rim_loop_blend(
    snapshot: SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, RimLoopBlendError> {
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(RimLoopBlendError::TargetInvalid);
    }
    let prism =
        extract_prism(topology, precision).map_err(|_| RimLoopBlendError::DomainUnsupported)?;
    if !prism.holes().is_empty() {
        return Err(RimLoopBlendError::DomainUnsupported);
    }
    if !distance.is_finite()
        || distance < precision.min_feature_size
        || distance >= prism.height() - precision.min_feature_size
    {
        return Err(RimLoopBlendError::DistanceInvalid);
    }
    let top = resolve_complete_cap_loop(topology, &prism, targets, precision)?;
    // The bottom rim is the top rim of the same prism viewed from the far
    // cap, so mirroring the profile lets one builder serve both.
    let mirrored = (!top).then(|| prism.mirrored());
    let prism = mirrored.as_ref().unwrap_or(&prism);

    let spine =
        mitred_inward_offset(prism.outer(), distance, precision).map_err(|error| match error {
            LoopOffsetError::ReflexSharpCorner => RimLoopBlendError::ReflexCorner,
            LoopOffsetError::RadiusTooLarge | LoopOffsetError::SelfIntersects => {
                RimLoopBlendError::DistanceInvalid
            }
            LoopOffsetError::Degenerate => RimLoopBlendError::DomainUnsupported,
        })?;
    let blended = match kind {
        EdgeFinishKind::Chamfer => build_chamfered_prism(prism, &spine, distance, precision),
        EdgeFinishKind::Fillet => build_filleted_prism(prism, &spine, distance, precision),
    }?;
    Ok(if top {
        blended
    } else {
        // The mirrored build names the geometric bottom "top"; restore the
        // roles so the caps keep the extrusion's own sense.
        swap_cap_roles(blended)
    })
}

/// Exchanges the two cap roles of a mirrored build.
fn swap_cap_roles(mut topology: Topology) -> Topology {
    for face in &mut topology.faces {
        face.value.role = match face.value.role {
            FaceRole::ExtrusionTop => FaceRole::ExtrusionBottom,
            FaceRole::ExtrusionBottom => FaceRole::ExtrusionTop,
            other => other,
        };
    }
    topology
}

/// Confirms the targets are exactly one cap's complete outer rim, and reports
/// whether that cap is the top one.
fn resolve_complete_cap_loop(
    topology: &Topology,
    prism: &PrismProfile,
    targets: &[EntityRef],
    precision: PrecisionPolicy,
) -> Result<bool, RimLoopBlendError> {
    let agreement = precision.linear_agreement.max(1.0e-9) * (1.0 + prism.height().abs());
    let mut heights = Vec::with_capacity(targets.len());
    for target in targets {
        let edge = topology
            .edges
            .iter()
            .find(|edge| edge.id.get() == target.entity.0)
            .ok_or(RimLoopBlendError::TargetInvalid)?;
        let [start, end] = edge.value.endpoints();
        let start_height = (start - prism.frame().origin).dot(prism.frame().normal);
        let end_height = (end - prism.frame().origin).dot(prism.frame().normal);
        if (start_height - end_height).abs() > agreement {
            // A generator rather than a rim edge.
            return Err(RimLoopBlendError::DomainUnsupported);
        }
        heights.push(start_height);
    }
    let first = heights[0];
    if heights
        .iter()
        .any(|height| (height - first).abs() > agreement)
    {
        return Err(RimLoopBlendError::DomainUnsupported);
    }
    // The whole loop must be selected: one rim edge per profile segment.
    if targets.len() != prism.outer().len() {
        return Err(RimLoopBlendError::DomainUnsupported);
    }
    let top = (first - prism.height()).abs() <= agreement;
    let bottom = first.abs() <= agreement;
    if !top && !bottom {
        return Err(RimLoopBlendError::DomainUnsupported);
    }
    Ok(top)
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

struct Builder<'a> {
    topology: Topology,
    next_id: u64,
    prism: &'a PrismProfile,
}

impl Builder<'_> {
    fn allocate(&mut self) -> EntityId {
        let id = EntityId::from_raw(self.next_id);
        self.next_id += 1;
        id
    }

    fn world(&self, planar: Point2, height: f64) -> Point3 {
        let frame = self.prism.frame();
        frame.origin + frame.u * planar.x + frame.v * planar.y + frame.normal * height
    }

    fn vertex(&mut self, point: Point3) -> VertexKey {
        let key = VertexKey(self.topology.vertices.len());
        let id = self.allocate();
        self.topology.vertices.push(Record {
            id,
            value: Vertex { point },
        });
        key
    }

    fn line_edge(&mut self, from: VertexKey, to: VertexKey) -> EdgeKey {
        let start = self.topology.vertices[from.0].value.point;
        let end = self.topology.vertices[to.0].value.point;
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record {
            id,
            value: Edge::line([from, to], [start, end]),
        });
        key
    }

    /// A planar direction lifted into world space, optionally reversed.
    fn direction(&self, planar: Point2, sign: f64) -> Vector3 {
        let frame = self.prism.frame();
        (frame.u * planar.x + frame.v * planar.y) * sign
    }

    fn arc_edge(
        &mut self,
        vertices: [VertexKey; 2],
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
        range: (f64, f64),
    ) -> EdgeKey {
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record {
            id,
            value: Edge {
                vertices,
                curve: Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                },
                parameter_range: ParameterRange::new(range.0, range.1),
            },
        });
        key
    }

    /// Wraps the accumulated faces in one shell and solid.
    fn finish(mut self) -> Topology {
        let shell_key = ShellKey(self.topology.shells.len());
        let shell_id = self.allocate();
        let face_count = self.topology.faces.len();
        self.topology.shells.push(Record {
            id: shell_id,
            value: Shell {
                faces: (0..face_count).map(FaceKey).collect(),
            },
        });
        let solid_id = self.allocate();
        self.topology.solids.push(Record {
            id: solid_id,
            value: Solid {
                outer_shell: shell_key,
                inner_shells: Vec::new(),
            },
        });
        self.topology
    }

    /// An edge following the profile at `height`, optionally trimmed to a
    /// sub-span of it.
    fn profile_edge(
        &mut self,
        band: SweptBand,
        vertices: [VertexKey; 2],
        height: f64,
        span: Option<(Point2, Point2)>,
    ) -> EdgeKey {
        match band {
            SweptBand::Straight { .. } => self.line_edge(vertices[0], vertices[1]),
            SweptBand::Revolved { center, radius, .. } => {
                let (from, to) = span.map_or_else(
                    || {
                        (
                            self.topology.vertices[vertices[0].0].value.point,
                            self.topology.vertices[vertices[1].0].value.point,
                        )
                    },
                    |(from, to)| (self.world(from, height), self.world(to, height)),
                );
                let world_center = self.world(center, height);
                let angle = |point: Point3| {
                    let frame = self.prism.frame();
                    let offset = point - world_center;
                    offset.dot(frame.v).atan2(offset.dot(frame.u))
                };
                let start = angle(from);
                let end = advance(start, angle(to), band);
                let frame = self.prism.frame();
                self.arc_edge(
                    vertices,
                    world_center,
                    frame.u,
                    frame.v,
                    radius,
                    (start, end),
                )
            }
        }
    }

    /// The cap-side counterpart of [`Self::profile_edge`], following the spine.
    fn spine_edge(
        &mut self,
        band: SweptBand,
        angles: (f64, f64),
        vertices: [VertexKey; 2],
        height: f64,
    ) -> EdgeKey {
        match band {
            SweptBand::Straight { .. } => self.line_edge(vertices[0], vertices[1]),
            SweptBand::Revolved {
                center,
                spine_radius,
                ..
            } => {
                let world_center = self.world(center, height);
                let frame = self.prism.frame();
                self.arc_edge(
                    vertices,
                    world_center,
                    frame.u,
                    frame.v,
                    spine_radius,
                    angles,
                )
            }
        }
    }

    /// The pcurve of a profile-following edge on a cap plane. `mirrored`
    /// selects the bottom cap's swapped frame.
    fn cap_pcurve(
        &self,
        band: SweptBand,
        segment: Segment,
        angles: (f64, f64),
        mirrored: bool,
    ) -> (Curve2, ParameterRange) {
        let map = |point: Point2| {
            if mirrored {
                Point2::new(point.y, point.x)
            } else {
                point
            }
        };
        match band {
            SweptBand::Straight { .. } => {
                let (from, to) = if mirrored {
                    (map(segment.end()), map(segment.start()))
                } else {
                    (map(segment.start()), map(segment.end()))
                };
                Curve2::line_segment([from, to])
            }
            SweptBand::Revolved { center, .. } => {
                let radius = match segment {
                    Segment::Arc { radius, .. } => radius,
                    Segment::Line { .. } => 0.0,
                };
                let (u, v) = if mirrored {
                    (Vector2::new(0.0, 1.0), Vector2::new(1.0, 0.0))
                } else {
                    (Vector2::new(1.0, 0.0), Vector2::new(0.0, 1.0))
                };
                let range = ParameterRange::new(angles.0, angles.1);
                (
                    Curve2::Circle {
                        center: map(center),
                        u,
                        v,
                        radius,
                    },
                    if mirrored { range.reversed() } else { range },
                )
            }
        }
    }

    /// The wall's parameter extent: arc length for a run, azimuth for an arc.
    fn wall_extent(&self, band: SweptBand, segment: Segment) -> (f64, f64) {
        match band {
            SweptBand::Straight { .. } => (
                0.0,
                (segment.end().x - segment.start().x).hypot(segment.end().y - segment.start().y),
            ),
            SweptBand::Revolved { angles, .. } => {
                (angles.0 * band.sense(), angles.1 * band.sense())
            }
        }
    }

    /// Where a point on the profile lands in the wall's parameter space.
    fn wall_parameter_of(&self, band: SweptBand, segment: Segment, point: Point2) -> f64 {
        match band {
            SweptBand::Straight { .. } => {
                let start = segment.start();
                (point.x - start.x).hypot(point.y - start.y)
            }
            SweptBand::Revolved { center, angles, .. } => {
                let angle = (point.y - center.y).atan2(point.x - center.x);
                advance(angles.0, angle, band) * band.sense()
            }
        }
    }

    fn wall_surface(&self, band: SweptBand, segment: Segment) -> Surface {
        let frame = self.prism.frame();
        match band {
            SweptBand::Straight { direction, .. } => {
                let origin = self.world(segment.start(), 0.0);
                let along = self.direction(direction, 1.0);
                Surface::Plane(Plane::new(origin, along, frame.normal))
            }
            SweptBand::Revolved { center, radius, .. } => Surface::Cylinder(Cylinder {
                origin: self.world(center, 0.0),
                axis: frame.normal,
                radial_u: frame.u,
                radial_v: frame.v,
                radius,
                angular_sign: band.sense(),
            }),
        }
    }

    /// The blend surface swept by the rolling ball over one segment.
    fn band_surface(&self, band: SweptBand, spine: Segment, wall_top: f64, fillet: f64) -> Surface {
        let frame = self.prism.frame();
        match band {
            SweptBand::Straight { direction, inward } => Surface::Cylinder(Cylinder {
                origin: self.world(spine.start(), wall_top),
                axis: self.direction(direction, 1.0),
                radial_u: frame.normal,
                radial_v: self.direction(inward, -1.0),
                radius: fillet,
                angular_sign: 1.0,
            }),
            SweptBand::Revolved {
                center,
                spine_radius,
                convex,
                ..
            } => Surface::Torus(Torus {
                origin: self.world(center, wall_top),
                axis: frame.normal * convex,
                radial_u: frame.u,
                radial_v: frame.v * convex,
                major_radius: spine_radius,
                minor_radius: fillet,
                angular_sign: 1.0,
            }),
        }
    }

    /// The band's boundary in its own parameter space. A cylinder measures the
    /// blend angle from the cap; a torus measures it from the wall.
    #[allow(clippy::too_many_arguments)]
    fn band_uses(
        &self,
        band: SweptBand,
        wall_edge: EdgeKey,
        end_seam: EdgeKey,
        cap_edge: EdgeKey,
        start_seam: EdgeKey,
        span: (f64, f64),
    ) -> Vec<(EdgeKey, Orientation, Curve2, ParameterRange)> {
        let (low, high) = span;
        let (wall, cap) = band.minor_parameters();
        // A cylinder sweeps its blend angle across `x` and its length along
        // `y`; a torus sweeps azimuth across `x` and the blend angle along `y`.
        let point = |blend: f64, along: f64| match band {
            SweptBand::Straight { .. } => Point2::new(blend, along),
            SweptBand::Revolved { .. } => Point2::new(along, blend),
        };
        vec![
            {
                let (curve, range) = Curve2::line_segment([point(wall, low), point(wall, high)]);
                (wall_edge, Orientation::Forward, curve, range)
            },
            {
                let (curve, range) = Curve2::line_segment([point(wall, high), point(cap, high)]);
                (end_seam, Orientation::Forward, curve, range)
            },
            {
                let (curve, range) = Curve2::line_segment([point(cap, high), point(cap, low)]);
                (cap_edge, Orientation::Reverse, curve, range)
            },
            {
                let (curve, range) = Curve2::line_segment([point(cap, low), point(wall, low)]);
                (start_seam, Orientation::Reverse, curve, range)
            },
        ]
    }

    fn push_loop(&mut self, uses: Vec<(EdgeKey, Orientation, Curve2, ParameterRange)>) -> LoopKey {
        let mut coedges = Vec::with_capacity(uses.len());
        for (edge, orientation, pcurve, range) in uses {
            let key = CoedgeKey(self.topology.coedges.len());
            let id = self.allocate();
            self.topology.coedges.push(Record {
                id,
                value: Coedge {
                    edge,
                    orientation,
                    pcurve,
                    parameter_range: range,
                },
            });
            coedges.push(key);
        }
        let key = LoopKey(self.topology.loops.len());
        let id = self.allocate();
        self.topology.loops.push(Record {
            id,
            value: Loop { coedges },
        });
        key
    }

    fn push_face(&mut self, surface: Surface, outer_loop: LoopKey, role: FaceRole) {
        let id = self.allocate();
        self.topology.faces.push(Record {
            id,
            value: Face {
                surface,
                outer_loop,
                inner_loops: Vec::new(),
                role,
            },
        });
    }
}

fn pcurve(from: Point2, to: Point2) -> (Curve2, ParameterRange) {
    Curve2::line_segment([from, to])
}

/// A pcurve following the profile carrier between two of its points, in the
/// profile's own coordinates. The ledge plane uses exactly that frame, so an
/// arc-carried setback stays a circle there rather than a chord.
fn carrier_pcurve(band: SweptBand, from: Point2, to: Point2) -> (Curve2, ParameterRange) {
    match band {
        SweptBand::Straight { .. } => pcurve(from, to),
        SweptBand::Revolved { center, radius, .. } => {
            let angle = |point: Point2| (point.y - center.y).atan2(point.x - center.x);
            let start = angle(from);
            (
                Curve2::Circle {
                    center,
                    u: Vector2::new(1.0, 0.0),
                    v: Vector2::new(0.0, 1.0),
                    radius,
                },
                ParameterRange::new(start, advance(start, angle(to), band)),
            )
        }
    }
}

/// The wall point directly outward of a spine point: the rolling ball's
/// tangency line on the wall. This is the setback at a sharp corner and the
/// junction point itself at a tangent one, on straight and arc carriers alike.
fn wall_point(band: SweptBand, spine_point: Point2, fillet: f64) -> Point2 {
    match band {
        SweptBand::Straight { inward, .. } => offset_by(spine_point, inward, -fillet),
        SweptBand::Revolved { center, radius, .. } => {
            let offset = Point2::new(spine_point.x - center.x, spine_point.y - center.y);
            let length = offset.x.hypot(offset.y);
            if length <= 0.0 {
                return spine_point;
            }
            Point2::new(
                center.x + offset.x * radius / length,
                center.y + offset.y * radius / length,
            )
        }
    }
}

/// Builds the chamfered prism: unchanged bottom cap and wall bases, a slant
/// per profile segment, and the cap shrunk to the spine. A straight segment
/// slants along a plane; an arc slants along a cone whose ring radius falls
/// from the profile radius at the wall top to the spine radius at the cap.
fn build_chamfered_prism(
    prism: &PrismProfile,
    spine: &SpineLoop,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, RimLoopBlendError> {
    let profile = prism.outer();
    let count = profile.len();
    let wall_top = prism.height() - distance;
    let frame = prism.frame();

    let bands: Vec<SweptBand> = (0..count)
        .map(|index| describe(profile[index], spine.segments[index]))
        .collect::<Option<Vec<_>>>()
        .ok_or(RimLoopBlendError::DomainUnsupported)?;
    // Two adjacent slants meet along a straight mitre only when both are
    // planar or the junction is tangent. A sharp junction touching an arc
    // would need the plane/cone intersection conic, which is outside the
    // curve vocabulary.
    for index in 0..count {
        let previous = (index + count - 1) % count;
        let leaving = bands[previous].normals().1;
        let entering = bands[index].normals().0;
        let turn = leaving
            .x
            .mul_add(entering.y, -(leaving.y * entering.x))
            .atan2(leaving.x.mul_add(entering.x, leaving.y * entering.y));
        let sharp = turn.abs() > precision.angular_agreement_radians.max(1.0e-9);
        if sharp
            && (matches!(bands[previous], SweptBand::Revolved { .. })
                || matches!(bands[index], SweptBand::Revolved { .. }))
        {
            return Err(RimLoopBlendError::DomainUnsupported);
        }
    }

    let mut builder = Builder {
        topology: Topology::default(),
        next_id: 1,
        prism,
    };

    // Vertices: profile corners at the base and at the wall top, and the
    // spine's mitre corners at the cap.
    let bottom: Vec<VertexKey> = (0..count)
        .map(|index| {
            let point = builder.world(profile[index].start(), 0.0);
            builder.vertex(point)
        })
        .collect();
    let wall: Vec<VertexKey> = (0..count)
        .map(|index| {
            let point = builder.world(profile[index].start(), wall_top);
            builder.vertex(point)
        })
        .collect();
    let cap: Vec<VertexKey> = (0..count)
        .map(|index| {
            let point = builder.world(spine.segments[index].start(), prism.height());
            builder.vertex(point)
        })
        .collect();

    // Edges.
    let base: Vec<EdgeKey> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            builder.profile_edge(bands[index], [bottom[index], bottom[next]], 0.0, None)
        })
        .collect();
    let vertical: Vec<EdgeKey> = (0..count)
        .map(|index| builder.line_edge(bottom[index], wall[index]))
        .collect();
    let wall_edge: Vec<EdgeKey> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            builder.profile_edge(bands[index], [wall[index], wall[next]], wall_top, None)
        })
        .collect();
    let mitre: Vec<EdgeKey> = (0..count)
        .map(|index| builder.line_edge(wall[index], cap[index]))
        .collect();
    let cap_edge: Vec<EdgeKey> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            builder.spine_edge(
                bands[index],
                bands[index].angles(),
                [cap[index], cap[next]],
                prism.height(),
            )
        })
        .collect();

    // Bottom cap: outward normal opposes the extrusion direction, so its frame
    // is mirrored and the loop runs backwards along the profile.
    let bottom_uses = (0..count)
        .rev()
        .map(|index| {
            let (curve, range) =
                builder.cap_pcurve(bands[index], profile[index], bands[index].angles(), true);
            (base[index], Orientation::Reverse, curve, range)
        })
        .collect::<Vec<_>>();
    let bottom_loop = builder.push_loop(bottom_uses);
    builder.push_face(
        Surface::Plane(Plane::new(frame.origin, frame.v, frame.u)),
        bottom_loop,
        FaceRole::ExtrusionBottom,
    );

    // Walls, unchanged below the chamfer.
    for index in 0..count {
        let next = (index + 1) % count;
        let (low, high) = builder.wall_extent(bands[index], profile[index]);
        let uses = vec![
            {
                let (curve, range) = pcurve(Point2::new(low, 0.0), Point2::new(high, 0.0));
                (base[index], Orientation::Forward, curve, range)
            },
            {
                let (curve, range) = pcurve(Point2::new(high, 0.0), Point2::new(high, wall_top));
                (vertical[next], Orientation::Forward, curve, range)
            },
            {
                let (curve, range) =
                    pcurve(Point2::new(high, wall_top), Point2::new(low, wall_top));
                (wall_edge[index], Orientation::Reverse, curve, range)
            },
            {
                let (curve, range) = pcurve(Point2::new(low, wall_top), Point2::new(low, 0.0));
                (vertical[index], Orientation::Reverse, curve, range)
            },
        ];
        let loop_key = builder.push_loop(uses);
        let surface = builder.wall_surface(bands[index], profile[index]);
        builder.push_face(
            surface,
            loop_key,
            FaceRole::ExtrusionSide(u32::try_from(index).unwrap_or(u32::MAX)),
        );
    }

    // Slants. Over a run the profile line at the wall top and the spine line
    // at the cap are parallel, so the slant is exactly planar; over an arc the
    // two are concentric circles of different radii, so the slant is a cone.
    for index in 0..count {
        let next = (index + 1) % count;
        let (uses, surface) = match bands[index] {
            SweptBand::Straight { direction, inward } => {
                let start = profile[index].start();
                let end = profile[index].end();
                let length = distance_between(start, end);
                // The rise combines the inward offset with the axial climb.
                let rise = std::f64::consts::SQRT_2 * distance;
                let along_of = |point: Point2| {
                    (point.x - start.x).mul_add(direction.x, (point.y - start.y) * direction.y)
                };
                let cap_start = spine.segments[index].start();
                let cap_end = spine.segments[index].end();
                let uses = vec![
                    {
                        let (curve, range) =
                            pcurve(Point2::new(0.0, 0.0), Point2::new(length, 0.0));
                        (wall_edge[index], Orientation::Forward, curve, range)
                    },
                    {
                        let (curve, range) = pcurve(
                            Point2::new(length, 0.0),
                            Point2::new(along_of(cap_end), rise),
                        );
                        (mitre[next], Orientation::Forward, curve, range)
                    },
                    {
                        let (curve, range) = pcurve(
                            Point2::new(along_of(cap_end), rise),
                            Point2::new(along_of(cap_start), rise),
                        );
                        (cap_edge[index], Orientation::Reverse, curve, range)
                    },
                    {
                        let (curve, range) = pcurve(
                            Point2::new(along_of(cap_start), rise),
                            Point2::new(0.0, 0.0),
                        );
                        (mitre[index], Orientation::Reverse, curve, range)
                    },
                ];
                let origin = builder.world(start, wall_top);
                let along = builder.direction(direction, 1.0);
                let up = (builder.direction(inward, 1.0) + frame.normal)
                    * std::f64::consts::FRAC_1_SQRT_2;
                (uses, Surface::Plane(Plane::new(origin, along, up)))
            }
            SweptBand::Revolved {
                center,
                radius,
                spine_radius,
                angles,
                ..
            } => {
                // The cone measures azimuth across `u` and the climb from the
                // wall top along `v`, so its parameter domain is the rectangle
                // the band's four edges bound. A concave carrier is traversed
                // clockwise, so its surface reverses to keep the loop
                // counter-clockwise and the normal outward.
                let sense = bands[index].sense();
                let angles = (angles.0 * sense, angles.1 * sense);
                let uses = vec![
                    {
                        let (curve, range) =
                            pcurve(Point2::new(angles.0, 0.0), Point2::new(angles.1, 0.0));
                        (wall_edge[index], Orientation::Forward, curve, range)
                    },
                    {
                        let (curve, range) =
                            pcurve(Point2::new(angles.1, 0.0), Point2::new(angles.1, distance));
                        (mitre[next], Orientation::Forward, curve, range)
                    },
                    {
                        let (curve, range) = pcurve(
                            Point2::new(angles.1, distance),
                            Point2::new(angles.0, distance),
                        );
                        (cap_edge[index], Orientation::Reverse, curve, range)
                    },
                    {
                        let (curve, range) =
                            pcurve(Point2::new(angles.0, distance), Point2::new(angles.0, 0.0));
                        (mitre[index], Orientation::Reverse, curve, range)
                    },
                ];
                let surface = Surface::Cone(Cone {
                    origin: builder.world(center, wall_top),
                    axis: frame.normal,
                    radial_u: frame.u,
                    radial_v: frame.v,
                    base_radius: radius,
                    slope: (spine_radius - radius) / distance,
                    angular_sign: sense,
                });
                (uses, surface)
            }
        };
        let loop_key = builder.push_loop(uses);
        builder.push_face(
            surface,
            loop_key,
            FaceRole::FeatureSide(u32::try_from(index).unwrap_or(u32::MAX)),
        );
    }

    // Top cap, shrunk to the spine.
    let cap_uses = (0..count)
        .map(|index| {
            let (curve, range) = builder.cap_pcurve(
                bands[index],
                spine.segments[index],
                bands[index].angles(),
                false,
            );
            (cap_edge[index], Orientation::Forward, curve, range)
        })
        .collect::<Vec<_>>();
    let cap_loop = builder.push_loop(cap_uses);
    builder.push_face(
        Surface::Plane(Plane::new(
            frame.origin + frame.normal * prism.height(),
            frame.u,
            frame.v,
        )),
        cap_loop,
        FaceRole::ExtrusionTop,
    );

    Ok(builder.finish())
}

/// Moves `angle` into the half-turn range that follows `from` in the band's
/// own sense, so a clockwise (concave) band keeps a decreasing sweep instead
/// of jumping a whole turn at the branch cut.
fn advance(from: f64, angle: f64, band: SweptBand) -> f64 {
    let tau = std::f64::consts::TAU;
    let mut angle = angle;
    let clockwise = matches!(band, SweptBand::Revolved { convex, .. } if convex < 0.0);
    if clockwise {
        while angle > from + 1.0e-9 {
            angle -= tau;
        }
    } else {
        while angle < from - 1.0e-9 {
            angle += tau;
        }
    }
    angle
}

fn planar_direction(start: Point2, end: Point2) -> Point2 {
    let length = (end.x - start.x).hypot(end.y - start.y);
    if length <= 0.0 {
        return Point2::new(1.0, 0.0);
    }
    Point2::new((end.x - start.x) / length, (end.y - start.y) / length)
}

/// What a profile segment sweeps: a straight run gives a quarter cylinder,
/// an arc gives a quarter torus about the same centre.
#[derive(Clone, Copy)]
enum SweptBand {
    Straight {
        direction: Point2,
        inward: Point2,
    },
    Revolved {
        center: Point2,
        radius: f64,
        spine_radius: f64,
        /// `+1` when the arc bulges outward, `-1` when it is concave.
        convex: f64,
        angles: (f64, f64),
    },
}

impl SweptBand {
    /// The inward normal where the segment starts and where it ends.
    fn normals(self) -> (Point2, Point2) {
        match self {
            Self::Straight { inward, .. } => (inward, inward),
            Self::Revolved { convex, angles, .. } => {
                let normal = |angle: f64| Point2::new(-convex * angle.cos(), -convex * angle.sin());
                (normal(angles.0), normal(angles.1))
            }
        }
    }

    /// The band's untrimmed parameter span: azimuth over an arc, and an
    /// unused zero pair over a run.
    const fn angles(self) -> (f64, f64) {
        match self {
            Self::Straight { .. } => (0.0, 0.0),
            Self::Revolved { angles, .. } => angles,
        }
    }

    /// `+1` when the band's surface measures azimuth the same way the profile
    /// does, `-1` when it runs the other way. A concave carrier is traversed
    /// clockwise, and every face's parameter loop must still wind
    /// counter-clockwise, so its surface reverses.
    const fn sense(self) -> f64 {
        match self {
            Self::Straight { .. } => 1.0,
            Self::Revolved { convex, .. } => convex,
        }
    }

    /// The minor-angle parameters of the wall and cap tangencies on a band.
    /// A concave band's frame is flipped so that its cap sits above its wall
    /// in parameter space as well as in the model.
    const fn minor_parameters(self) -> (f64, f64) {
        let quarter = std::f64::consts::FRAC_PI_2;
        match self {
            Self::Straight { .. } => (quarter, 0.0),
            Self::Revolved { convex, .. } => {
                if convex > 0.0 {
                    (0.0, quarter)
                } else {
                    (-std::f64::consts::PI, -quarter)
                }
            }
        }
    }
}

fn describe(profile: Segment, spine: Segment) -> Option<SweptBand> {
    match (profile, spine) {
        (Segment::Line { start, end }, Segment::Line { .. }) => {
            let direction = planar_direction(start, end);
            Some(SweptBand::Straight {
                direction,
                inward: Point2::new(-direction.y, direction.x),
            })
        }
        (
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            },
            Segment::Arc {
                radius: spine_radius,
                ..
            },
        ) => Some(SweptBand::Revolved {
            center,
            radius,
            spine_radius,
            convex: if sweep >= 0.0 { 1.0 } else { -1.0 },
            angles: (start_angle, start_angle + sweep),
        }),
        _ => None,
    }
}

/// Builds the filleted prism: unchanged walls below `h - f`, a quarter
/// cylinder or quarter torus over every profile segment, and a sphere patch
/// plus a flat ledge at every sharp corner.
fn build_filleted_prism(
    prism: &PrismProfile,
    spine: &SpineLoop,
    fillet: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, RimLoopBlendError> {
    let profile = prism.outer();
    let count = profile.len();
    let wall_top = prism.height() - fillet;
    let frame = prism.frame();

    // What each segment sweeps, and where its band meets the wall.
    let bands: Vec<SweptBand> = (0..count)
        .map(|index| describe(profile[index], spine.segments[index]))
        .collect::<Option<Vec<_>>>()
        .ok_or(RimLoopBlendError::DomainUnsupported)?;
    // A concave arc keeps its own carrier: its band grows to `r + f` and its
    // wall runs clockwise, which the loop winding already expresses.

    // Tangent junctions need no corner treatment: the neighbouring bands
    // already meet along one shared seam arc.
    let sharp: Vec<bool> = (0..count)
        .map(|index| {
            let previous = (index + count - 1) % count;
            let leaving = bands[previous].normals().1;
            let entering = bands[index].normals().0;
            let turn = leaving
                .x
                .mul_add(entering.y, -(leaving.y * entering.x))
                .atan2(leaving.x.mul_add(entering.x, leaving.y * entering.y));
            turn.abs() > precision.angular_agreement_radians.max(1.0e-9)
        })
        .collect();
    // Band tangency points: the wall point directly outward of each spine
    // endpoint. At a sharp corner the spine is trimmed back to its mitre, so
    // this is the setback; at a tangent junction it is the junction itself.
    let band_start: Vec<Point2> = (0..count)
        .map(|index| wall_point(bands[index], spine.segments[index].start(), fillet))
        .collect();
    let band_end: Vec<Point2> = (0..count)
        .map(|index| wall_point(bands[index], spine.segments[index].end(), fillet))
        .collect();
    // The inward normal at each tangency point. On an arc that is the trimmed
    // azimuth's normal, which is what the seams and the sphere patch need; on
    // a run it is the segment normal either way.
    let inward_at = |band: SweptBand, wall: Point2, centre: Point2| match band {
        SweptBand::Straight { inward, .. } => inward,
        SweptBand::Revolved { .. } => {
            Point2::new((centre.x - wall.x) / fillet, (centre.y - wall.y) / fillet)
        }
    };
    let entering: Vec<Point2> = (0..count)
        .map(|index| {
            inward_at(
                bands[index],
                band_start[index],
                spine.segments[index].start(),
            )
        })
        .collect();
    let leaving: Vec<Point2> = (0..count)
        .map(|index| inward_at(bands[index], band_end[index], spine.segments[index].end()))
        .collect();
    // Each band's trimmed parameter span: arc length over a run, azimuth over
    // an arc. The spine shares it, the two carriers being concentric.
    let span: Vec<(f64, f64)> = (0..count)
        .map(|index| match bands[index] {
            SweptBand::Straight { .. } => {
                (0.0, distance_between(band_start[index], band_end[index]))
            }
            SweptBand::Revolved { center, .. } => {
                let angle = |point: Point2| (point.y - center.y).atan2(point.x - center.x);
                let low = angle(band_start[index]);
                (low, advance(low, angle(band_end[index]), bands[index]))
            }
        })
        .collect();

    // Each sharp corner must leave a usable setback on both neighbours, and
    // every band must retain a usable span.
    for index in 0..count {
        let previous = (index + count - 1) % count;
        if sharp[index] {
            let corner = profile[index].start();
            let lead = distance_between(corner, band_start[index]);
            let trail = distance_between(band_end[previous], corner);
            if lead < precision.min_feature_size || trail < precision.min_feature_size {
                return Err(RimLoopBlendError::DistanceInvalid);
            }
        }
        if distance_between(band_start[index], band_end[index]) < precision.min_feature_size
            && matches!(bands[index], SweptBand::Straight { .. })
        {
            return Err(RimLoopBlendError::DistanceInvalid);
        }
    }

    let mut builder = Builder {
        topology: Topology::default(),
        next_id: 1,
        prism,
    };

    // Vertices. A tangent junction contributes one point, so its band start,
    // band end, and wall corner are the same vertex.
    let bottom: Vec<VertexKey> = (0..count)
        .map(|index| {
            let point = builder.world(profile[index].start(), 0.0);
            builder.vertex(point)
        })
        .collect();
    let wall_corner: Vec<VertexKey> = (0..count)
        .map(|index| {
            let point = builder.world(profile[index].start(), wall_top);
            builder.vertex(point)
        })
        .collect();
    let start_vertex: Vec<VertexKey> = (0..count)
        .map(|index| {
            if sharp[index] {
                let point = builder.world(band_start[index], wall_top);
                builder.vertex(point)
            } else {
                wall_corner[index]
            }
        })
        .collect();
    let end_vertex: Vec<VertexKey> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            if sharp[next] {
                let point = builder.world(band_end[index], wall_top);
                builder.vertex(point)
            } else {
                wall_corner[next]
            }
        })
        .collect();
    let cap: Vec<VertexKey> = (0..count)
        .map(|index| {
            let point = builder.world(spine.segments[index].start(), prism.height());
            builder.vertex(point)
        })
        .collect();

    // Profile-following edges: straight over a run, circular over an arc.
    let base: Vec<EdgeKey> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            builder.profile_edge(bands[index], [bottom[index], bottom[next]], 0.0, None)
        })
        .collect();
    let vertical: Vec<EdgeKey> = (0..count)
        .map(|index| builder.line_edge(bottom[index], wall_corner[index]))
        .collect();
    let lead: Vec<Option<EdgeKey>> = (0..count)
        .map(|index| {
            sharp[index].then(|| {
                builder.profile_edge(
                    bands[index],
                    [wall_corner[index], start_vertex[index]],
                    wall_top,
                    Some((profile[index].start(), band_start[index])),
                )
            })
        })
        .collect();
    let band_wall: Vec<EdgeKey> = (0..count)
        .map(|index| {
            builder.profile_edge(
                bands[index],
                [start_vertex[index], end_vertex[index]],
                wall_top,
                Some((band_start[index], band_end[index])),
            )
        })
        .collect();
    let trail: Vec<Option<EdgeKey>> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            sharp[next].then(|| {
                builder.profile_edge(
                    bands[index],
                    [end_vertex[index], wall_corner[next]],
                    wall_top,
                    Some((band_end[index], profile[index].end())),
                )
            })
        })
        .collect();
    let cap_edge: Vec<EdgeKey> = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            builder.spine_edge(
                bands[index],
                span[index],
                [cap[index], cap[next]],
                prism.height(),
            )
        })
        .collect();

    // Seam arcs rise from a band tangency point to the pole above its
    // junction. A tangent junction shares one seam between both neighbours.
    let quarter = std::f64::consts::FRAC_PI_2;
    let mut start_seam: Vec<Option<EdgeKey>> = vec![None; count];
    let mut end_seam: Vec<Option<EdgeKey>> = vec![None; count];
    for index in 0..count {
        let previous = (index + count - 1) % count;
        let center = builder.world(spine.segments[index].start(), wall_top);
        let entering_axis = builder.direction(entering[index], -1.0);
        if sharp[index] {
            let leaving_axis = builder.direction(leaving[previous], -1.0);
            end_seam[previous] = Some(builder.arc_edge(
                [end_vertex[previous], cap[index]],
                center,
                leaving_axis,
                frame.normal,
                fillet,
                (0.0, quarter),
            ));
            start_seam[index] = Some(builder.arc_edge(
                [start_vertex[index], cap[index]],
                center,
                entering_axis,
                frame.normal,
                fillet,
                (0.0, quarter),
            ));
        } else {
            let shared = builder.arc_edge(
                [start_vertex[index], cap[index]],
                center,
                entering_axis,
                frame.normal,
                fillet,
                (0.0, quarter),
            );
            start_seam[index] = Some(shared);
            end_seam[previous] = Some(shared);
        }
    }

    // Equator arcs bound each sphere patch below.
    let turn: Vec<f64> = (0..count)
        .map(|index| {
            let previous = (index + count - 1) % count;
            turn_angle(leaving[previous], entering[index])
        })
        .collect();
    let equator: Vec<Option<EdgeKey>> = (0..count)
        .map(|index| {
            sharp[index].then(|| {
                let previous = (index + count - 1) % count;
                let center = builder.world(spine.segments[index].start(), wall_top);
                let radial = builder.direction(leaving[previous], -1.0);
                let tangent = frame.normal.cross(radial);
                builder.arc_edge(
                    [end_vertex[previous], start_vertex[index]],
                    center,
                    radial,
                    tangent,
                    fillet,
                    (0.0, turn[index]),
                )
            })
        })
        .collect();

    // Bottom cap.
    let bottom_uses = (0..count)
        .rev()
        .map(|index| {
            let (curve, range) =
                builder.cap_pcurve(bands[index], profile[index], bands[index].angles(), true);
            (base[index], Orientation::Reverse, curve, range)
        })
        .collect::<Vec<_>>();
    let bottom_loop = builder.push_loop(bottom_uses);
    builder.push_face(
        Surface::Plane(Plane::new(frame.origin, frame.v, frame.u)),
        bottom_loop,
        FaceRole::ExtrusionBottom,
    );

    // Walls, whose tops are split by any sharp corner at either end.
    for index in 0..count {
        let next = (index + 1) % count;
        let (low, high) = builder.wall_extent(bands[index], profile[index]);
        let along_start =
            builder.wall_parameter_of(bands[index], profile[index], band_start[index]);
        let along_end = builder.wall_parameter_of(bands[index], profile[index], band_end[index]);
        let mut uses = vec![
            {
                let (curve, range) = pcurve(Point2::new(low, 0.0), Point2::new(high, 0.0));
                (base[index], Orientation::Forward, curve, range)
            },
            {
                let (curve, range) = pcurve(Point2::new(high, 0.0), Point2::new(high, wall_top));
                (vertical[next], Orientation::Forward, curve, range)
            },
        ];
        if let Some(edge) = trail[index] {
            let (curve, range) = pcurve(
                Point2::new(high, wall_top),
                Point2::new(along_end, wall_top),
            );
            uses.push((edge, Orientation::Reverse, curve, range));
        }
        {
            let (curve, range) = pcurve(
                Point2::new(along_end, wall_top),
                Point2::new(along_start, wall_top),
            );
            uses.push((band_wall[index], Orientation::Reverse, curve, range));
        }
        if let Some(edge) = lead[index] {
            let (curve, range) = pcurve(
                Point2::new(along_start, wall_top),
                Point2::new(low, wall_top),
            );
            uses.push((edge, Orientation::Reverse, curve, range));
        }
        {
            let (curve, range) = pcurve(Point2::new(low, wall_top), Point2::new(low, 0.0));
            uses.push((vertical[index], Orientation::Reverse, curve, range));
        }
        let loop_key = builder.push_loop(uses);
        let surface = builder.wall_surface(bands[index], profile[index]);
        builder.push_face(
            surface,
            loop_key,
            FaceRole::ExtrusionSide(u32::try_from(index).unwrap_or(u32::MAX)),
        );
    }

    // Bands: a quarter cylinder over a run, a quarter torus over an arc.
    for index in 0..count {
        let uses = builder.band_uses(
            bands[index],
            band_wall[index],
            end_seam[index].expect("every band has an end seam"),
            cap_edge[index],
            start_seam[index].expect("every band has a start seam"),
            (
                span[index].0 * bands[index].sense(),
                span[index].1 * bands[index].sense(),
            ),
        );
        let loop_key = builder.push_loop(uses);
        let surface = builder.band_surface(bands[index], spine.segments[index], wall_top, fillet);
        builder.push_face(
            surface,
            loop_key,
            FaceRole::FeatureSide(u32::try_from(index).unwrap_or(u32::MAX)),
        );
    }

    // Sphere patches and their ledges, at sharp corners only.
    let ledge_plane = Plane::new(frame.origin + frame.normal * wall_top, frame.u, frame.v);
    for index in 0..count {
        if !sharp[index] {
            continue;
        }
        let previous = (index + count - 1) % count;
        let sweep = turn[index];
        let equator_edge = equator[index].expect("a sharp corner has an equator");
        let uses = vec![
            {
                let (curve, range) = pcurve(Point2::new(0.0, 0.0), Point2::new(sweep, 0.0));
                (equator_edge, Orientation::Forward, curve, range)
            },
            {
                let (curve, range) = pcurve(Point2::new(sweep, 0.0), Point2::new(sweep, quarter));
                (
                    start_seam[index].expect("a sharp corner has a start seam"),
                    Orientation::Forward,
                    curve,
                    range,
                )
            },
            {
                let (curve, range) = pcurve(Point2::new(0.0, quarter), Point2::new(0.0, 0.0));
                (
                    end_seam[previous].expect("a sharp corner has an end seam"),
                    Orientation::Reverse,
                    curve,
                    range,
                )
            },
        ];
        let loop_key = builder.push_loop(uses);
        let origin = builder.world(spine.segments[index].start(), wall_top);
        let radial = builder.direction(leaving[previous], -1.0);
        let tangent = frame.normal.cross(radial);
        builder.push_face(
            Surface::Sphere(Sphere {
                origin,
                axis: frame.normal,
                radial_u: radial,
                radial_v: tangent,
                radius: fillet,
                angular_sign: 1.0,
            }),
            loop_key,
            FaceRole::FeatureEnd,
        );

        let corner = profile[index].start();
        let center = spine.segments[index].start();
        let normal = leaving[previous];
        let radial_2d = Point2::new(-normal.x, -normal.y);
        let tangent_2d = Point2::new(-radial_2d.y, radial_2d.x);
        let uses = vec![
            {
                let (curve, range) = carrier_pcurve(bands[previous], band_end[previous], corner);
                (
                    trail[previous].expect("a sharp corner has a trail"),
                    Orientation::Forward,
                    curve,
                    range,
                )
            },
            {
                let (curve, range) = carrier_pcurve(bands[index], corner, band_start[index]);
                (
                    lead[index].expect("a sharp corner has a lead"),
                    Orientation::Forward,
                    curve,
                    range,
                )
            },
            (
                equator_edge,
                Orientation::Reverse,
                Curve2::Circle {
                    center,
                    u: Vector2::new(radial_2d.x, radial_2d.y),
                    v: Vector2::new(tangent_2d.x, tangent_2d.y),
                    radius: fillet,
                },
                ParameterRange::new(turn[index], 0.0),
            ),
        ];
        let loop_key = builder.push_loop(uses);
        builder.push_face(
            Surface::Plane(ledge_plane),
            loop_key,
            FaceRole::FeatureSide(u32::try_from(count + index).unwrap_or(u32::MAX)),
        );
    }

    // Top cap, shrunk to the spine.
    let cap_uses = (0..count)
        .map(|index| {
            let (curve, range) =
                builder.cap_pcurve(bands[index], spine.segments[index], span[index], false);
            (cap_edge[index], Orientation::Forward, curve, range)
        })
        .collect::<Vec<_>>();
    let cap_loop = builder.push_loop(cap_uses);
    builder.push_face(
        Surface::Plane(Plane::new(
            frame.origin + frame.normal * prism.height(),
            frame.u,
            frame.v,
        )),
        cap_loop,
        FaceRole::ExtrusionTop,
    );

    Ok(builder.finish())
}

fn offset_by(point: Point2, direction: Point2, distance: f64) -> Point2 {
    Point2::new(
        direction.x.mul_add(distance, point.x),
        direction.y.mul_add(distance, point.y),
    )
}

fn distance_between(from: Point2, to: Point2) -> f64 {
    (to.x - from.x).hypot(to.y - from.y)
}

fn turn_angle(incoming: Point2, outgoing: Point2) -> f64 {
    let cross = incoming.x.mul_add(outgoing.y, -(incoming.y * outgoing.x));
    let dot = incoming.x.mul_add(outgoing.x, incoming.y * outgoing.y);
    cross.atan2(dot)
}

/// The complete outer rim loop of the cap face containing `edge`.
///
/// Interactive selection expands through this so a rim loop enters an
/// edge-set finish as one unit; a seed that is not on a cap outer loop falls
/// back to its analytic carrier group.
pub(crate) fn rim_loop_group(topology: &Topology, edge: EntityRef) -> Option<Vec<EntityRef>> {
    let edge_index = topology
        .edges
        .iter()
        .position(|record| record.id.get() == edge.entity.0)?;
    for face in &topology.faces {
        if !matches!(
            face.value.role,
            FaceRole::ExtrusionTop | FaceRole::ExtrusionBottom
        ) || face.value.surface.as_plane().is_none()
        {
            continue;
        }
        let loop_record = topology.loop_record(face.value.outer_loop)?;
        let members = loop_record
            .value
            .coedges
            .iter()
            .filter_map(|coedge_key| topology.coedge(*coedge_key))
            .map(|coedge| coedge.value.edge)
            .collect::<Vec<_>>();
        if members.iter().any(|member| member.0 == edge_index) {
            return Some(
                members
                    .into_iter()
                    .filter_map(|member| {
                        topology.edges.get(member.0).map(|record| EntityRef {
                            snapshot: edge.snapshot,
                            kind: EntityKind::Edge,
                            entity: artificer_protocol::EntityId(record.id.get()),
                        })
                    })
                    .collect(),
            );
        }
    }
    None
}

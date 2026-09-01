//! Live kernel session state, command execution, and transaction tracking.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EdgeFinishKind,
    ExecuteRequest, KernelCommand, OperationReport, PlanarAxis2, PlanarCurve2, PlanarFrame3,
    PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId,
    RevolveAngle, SnapshotId, Vector3,
};

use artificer_protocol::FaceExtrusionOperation;

use crate::commands::{ApiCommand, ExtrudeOp, SketchEntity, SketchPlane};
use crate::debug::{ApiError, ApiErrorCode, CommandResult, EntityInfo};
use crate::journal::{Journal, JournalEntry};
use crate::query::QueryHandle;
use crate::selectors::{resolve_selector, resolve_selector_set};
use crate::snapshot::{SnapshotOptions, SnapshotOutput, render_snapshot};

/// A stateful session owning the kernel instance, current snapshot, and history.
pub struct Session {
    pub kernel: NativeKernel,
    pub snapshot: Snapshot,
    pub snapshot_cache: BTreeMap<SnapshotId, Snapshot>,
    pub journal: Journal,
    pub precision: PrecisionPolicy,
    pub labels: BTreeMap<String, String>,
    pub step_order: Vec<String>,
    pub step_reports: BTreeMap<String, OperationReport>,
    pub step_snapshots: BTreeMap<String, SnapshotId>,
    pub undo_stack: Vec<(Snapshot, JournalEntry, Option<OperationReport>)>,
    pub redo_stack: Vec<JournalEntry>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::with_precision(PrecisionPolicy::default())
    }

    #[must_use]
    pub fn with_precision(precision: PrecisionPolicy) -> Self {
        let kernel = NativeKernel::new();
        let snapshot = NativeKernel::empty();
        let mut snapshot_cache = BTreeMap::new();
        snapshot_cache.insert(snapshot.id(), snapshot.clone());

        Self {
            kernel,
            snapshot,
            snapshot_cache,
            journal: Journal::new(),
            precision,
            labels: BTreeMap::new(),
            step_order: Vec::new(),
            step_reports: BTreeMap::new(),
            step_snapshots: BTreeMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn execute(
        &mut self,
        command: ApiCommand,
        token: &CancellationToken,
    ) -> Result<CommandResult, ApiError> {
        let start_time = Instant::now();
        let step_label = command.label().to_owned();
        if step_label.is_empty() || self.step_snapshots.contains_key(&step_label) {
            return Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                format!(
                    "Step label \"{step_label}\" is empty or already used; every step needs a unique label so later selectors can name it"
                ),
            ));
        }
        if let ApiCommand::Sketch { .. } = &command {
            return Ok(self.record_sketch(command, start_time));
        }

        let outcome: ExecutionOutcome = match &command {
            ApiCommand::BooleanUnion { target, tool, .. } => self.execute_boolean_op(
                target.0.as_str(),
                tool.0.as_str(),
                BooleanOperation::Union,
                token,
            )?,
            ApiCommand::BooleanDifference { target, tool, .. } => self.execute_boolean_op(
                target.0.as_str(),
                tool.0.as_str(),
                BooleanOperation::Difference,
                token,
            )?,
            ApiCommand::BooleanIntersection { target, tool, .. } => self.execute_boolean_op(
                target.0.as_str(),
                tool.0.as_str(),
                BooleanOperation::Intersection,
                token,
            )?,
            _ => {
                let kernel_cmd = self.lower_command(&command)?;
                let request = ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!("session::{step_label}")),
                    expected_snapshot: self.snapshot.id(),
                    precision: self.precision,
                    command: kernel_cmd,
                };
                NativeKernel::execute(&self.snapshot, &request, token).map_err(ApiError::from)?
            }
        };

        let elapsed_ms = start_time.elapsed().as_millis() as u64;

        // Extract newly generated / modified entities
        let mut entity_map = BTreeMap::new();
        for record in &outcome.report.history {
            if let Some(role) = &record.role {
                for (idx, out_entity) in record.outputs.iter().enumerate() {
                    let key = if let Some(ord) = role.ordinal {
                        format!("{}[{ord}]", role.name)
                    } else if record.outputs.len() > 1 {
                        format!("{}[{idx}]", role.name)
                    } else {
                        role.name.clone()
                    };
                    entity_map.insert(
                        key,
                        EntityInfo {
                            kind: out_entity.kind,
                            entity_ref: *out_entity,
                            geometry_description: format!(
                                "{:?} {}",
                                out_entity.kind, out_entity.entity
                            ),
                            role: Some(role.name.clone()),
                            ordinal: role.ordinal,
                        },
                    );
                }
            }
        }

        let result = CommandResult {
            success: outcome.report.validation.valid,
            step_label: step_label.clone(),
            snapshot_id: outcome.snapshot.id(),
            topology: outcome.snapshot.counts(),
            bounds: outcome.snapshot.measures().bounds,
            entities: entity_map,
            diagnostics: outcome.report.validation.diagnostics.clone(),
            elapsed_ms,
            summary: format!(
                "Step \"{}\" committed snapshot {}. {}",
                step_label,
                outcome.snapshot.id(),
                outcome.snapshot.counts()
            ),
        };

        // Record undo state, cache snapshot, and journal entry
        let entry = JournalEntry::new(command);
        self.undo_stack.push((
            self.snapshot.clone(),
            entry.clone(),
            Some(outcome.report.clone()),
        ));
        self.redo_stack.clear();

        self.step_order.push(step_label.clone());
        self.step_reports.insert(step_label.clone(), outcome.report);
        self.step_snapshots
            .insert(step_label, outcome.snapshot.id());
        self.snapshot_cache
            .insert(outcome.snapshot.id(), outcome.snapshot.clone());
        self.snapshot = outcome.snapshot;
        self.journal.push(entry);

        Ok(result)
    }

    /// A sketch is authoring intent rather than a kernel operation: it is
    /// journaled as a step of its own, leaves the snapshot untouched, and is
    /// consumed by the Extrude or Revolve that names it.
    fn record_sketch(&mut self, command: ApiCommand, start_time: Instant) -> CommandResult {
        let step_label = command.label().to_owned();
        let entry = JournalEntry::new(command);
        self.undo_stack
            .push((self.snapshot.clone(), entry.clone(), None));
        self.redo_stack.clear();
        self.step_order.push(step_label.clone());
        self.step_snapshots
            .insert(step_label.clone(), self.snapshot.id());
        self.journal.push(entry);
        CommandResult {
            success: true,
            step_label: step_label.clone(),
            snapshot_id: self.snapshot.id(),
            topology: self.snapshot.counts(),
            bounds: self.snapshot.measures().bounds,
            entities: BTreeMap::new(),
            diagnostics: Vec::new(),
            elapsed_ms: start_time.elapsed().as_millis() as u64,
            summary: format!(
                "Sketch \"{step_label}\" recorded; extrude or revolve it to build geometry."
            ),
        }
    }

    fn execute_boolean_op(
        &self,
        target_step: &str,
        tool_step: &str,
        operation: BooleanOperation,
        token: &CancellationToken,
    ) -> Result<ExecutionOutcome, ApiError> {
        let target_snap_id = self
            .step_snapshots
            .get(target_step)
            .copied()
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!("Target step \"{target_step}\" not found in session history"),
                )
            })?;
        let tool_snap_id = self.step_snapshots.get(tool_step).copied().ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SelectorNotFound,
                format!("Tool step \"{tool_step}\" not found in session history"),
            )
        })?;

        let target_snap = self.snapshot_cache.get(&target_snap_id).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SessionError,
                format!(
                    "Snapshot {target_snap_id} for target step \"{target_step}\" not found in cache"
                ),
            )
        })?;

        let tool_snap = self.snapshot_cache.get(&tool_snap_id).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SessionError,
                format!("Snapshot {tool_snap_id} for tool step \"{tool_step}\" not found in cache"),
            )
        })?;

        let request = BooleanRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("session::boolean::{operation:?}")),
            expected_target_snapshot: target_snap_id,
            expected_tool_snapshot: tool_snap_id,
            precision: self.precision,
            operation,
        };

        NativeKernel::execute_boolean(target_snap, tool_snap, &request, token)
            .map_err(ApiError::from)
    }

    fn lower_command(&self, cmd: &ApiCommand) -> Result<KernelCommand, ApiError> {
        match cmd {
            ApiCommand::MakeBox { origin, size, .. } => Ok(KernelCommand::MakeCuboid {
                origin: *origin,
                size_x: size[0],
                size_y: size[1],
                size_z: size[2],
            }),
            ApiCommand::MakeCylinder {
                center,
                axis,
                radius,
                height,
                ..
            } => {
                let norm = (axis.x * axis.x + axis.y * axis.y + axis.z * axis.z).sqrt();
                let axis_unit = if norm > 1e-9 {
                    Vector3::new(axis.x / norm, axis.y / norm, axis.z / norm)
                } else {
                    Vector3::new(0.0, 0.0, 1.0)
                };

                // Any world axis not parallel to the cylinder axis seeds the
                // frame; projecting its component along the axis away makes
                // `u` perpendicular, so the frame normal is the axis itself.
                let seed = if axis_unit.x.abs() < 0.9 {
                    Vector3::new(1.0, 0.0, 0.0)
                } else {
                    Vector3::new(0.0, 1.0, 0.0)
                };
                let along = seed.x * axis_unit.x + seed.y * axis_unit.y + seed.z * axis_unit.z;
                let u = Vector3::new(
                    seed.x - axis_unit.x * along,
                    seed.y - axis_unit.y * along,
                    seed.z - axis_unit.z * along,
                );
                let u_len = (u.x * u.x + u.y * u.y + u.z * u.z).sqrt();
                let u = Vector3::new(u.x / u_len, u.y / u_len, u.z / u_len);
                let v = Vector3::new(
                    axis_unit.y * u.z - axis_unit.z * u.y,
                    axis_unit.z * u.x - axis_unit.x * u.z,
                    axis_unit.x * u.y - axis_unit.y * u.x,
                );

                let frame = PlanarFrame3 {
                    origin: *center,
                    u,
                    v,
                };
                Ok(KernelCommand::MakeRevolvedAnnulus {
                    frame,
                    inner_radius: 0.0,
                    outer_radius: *radius,
                    height: *height,
                })
            }
            ApiCommand::PushPull { face, distance, .. } => {
                let target_face =
                    resolve_selector(face, &self.snapshot, &self.step_order, &self.step_reports)?;
                Ok(KernelCommand::PushPullFace {
                    target_face,
                    distance: *distance,
                })
            }
            ApiCommand::DrillHole {
                face,
                center,
                diameter,
                depth,
                ..
            } => {
                let target_face =
                    resolve_selector(face, &self.snapshot, &self.step_order, &self.step_reports)?;
                let support = NativeKernel::planar_face_support(&self.snapshot, target_face)
                    .map_err(ApiError::from)?;
                if !diameter.is_finite() || *diameter <= self.precision.min_feature_size * 2.0 {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "Hole diameter must exceed twice the minimum feature size",
                    ));
                }
                // A hole is a round cut through the face. Lowering it to the
                // general face cut, rather than the kernel's exact-only drill,
                // lets a hole that crosses earlier geometry fall to the
                // faceted tier with its approximation warning instead of
                // being refused outright.
                Ok(KernelCommand::ExtrudeFacePlanarProfile {
                    target_face,
                    frame: support.frame,
                    profile: PlanarProfile2 {
                        regions: vec![PlanarRegion2 {
                            outer: PlanarLoop2 {
                                curves: vec![PlanarCurve2::Circle {
                                    center: *center,
                                    radius: *diameter * 0.5,
                                    direction: ArcDirection::CounterClockwise,
                                }],
                            },
                            holes: Vec::new(),
                        }],
                    },
                    distance: *depth,
                    operation: FaceExtrusionOperation::Cut,
                })
            }
            ApiCommand::Fillet { edges, radius, .. } => {
                self.edge_finish(edges, EdgeFinishKind::Fillet, *radius)
            }
            ApiCommand::Chamfer {
                edges, distance, ..
            } => self.edge_finish(edges, EdgeFinishKind::Chamfer, *distance),
            ApiCommand::Mirror {
                plane_origin,
                plane_normal,
                ..
            } => Ok(KernelCommand::MirrorSnapshot {
                plane_origin: *plane_origin,
                plane_normal: *plane_normal,
            }),
            ApiCommand::LinearPattern {
                direction,
                spacing,
                count,
                ..
            } => Ok(KernelCommand::LinearPatternSnapshot {
                direction: *direction,
                spacing: *spacing,
                count: *count,
            }),
            ApiCommand::Sketch { .. } => unreachable!("a sketch is recorded, never lowered"),
            ApiCommand::Extrude {
                sketch,
                regions,
                distance,
                operation,
                ..
            } => {
                let (frame, profile) = self.build_sketch_profile(sketch)?;
                let profile = select_regions(profile, regions)?;
                match operation {
                    ExtrudeOp::New => Ok(KernelCommand::ExtrudePlanarProfile {
                        frame,
                        profile,
                        distance: *distance,
                    }),
                    ExtrudeOp::Add | ExtrudeOp::Cut => {
                        let target_face = self.sketch_face(sketch)?.ok_or_else(|| {
                            ApiError::new(
                                ApiErrorCode::InvalidInput,
                                "An add or cut extrusion needs a sketch drawn on a face of the body; a sketch on a world plane can only make a new body",
                            )
                        })?;
                        Ok(KernelCommand::ExtrudeFacePlanarProfile {
                            target_face,
                            frame,
                            profile,
                            distance: *distance,
                            operation: match operation {
                                ExtrudeOp::Add => FaceExtrusionOperation::Add,
                                _ => FaceExtrusionOperation::Cut,
                            },
                        })
                    }
                }
            }
            ApiCommand::Revolve {
                sketch,
                regions,
                axis_origin,
                axis_direction,
                angle_degrees,
                operation,
                ..
            } => {
                if *operation != ExtrudeOp::New {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "Revolve builds a new body; add and cut revolves are not supported",
                    ));
                }
                if (angle_degrees - 360.0).abs() > 1.0e-9 {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "Only a full 360 degree revolve is supported",
                    ));
                }
                let (frame, profile) = self.build_sketch_profile(sketch)?;
                let profile = select_regions(profile, regions)?;
                // The axis must lie in the sketch plane: project its origin
                // and direction into the frame and refuse anything that
                // leaves it.
                let relative = Vector3::new(
                    axis_origin.x - frame.origin.x,
                    axis_origin.y - frame.origin.y,
                    axis_origin.z - frame.origin.z,
                );
                let normal = Vector3::new(
                    frame.u.y * frame.v.z - frame.u.z * frame.v.y,
                    frame.u.z * frame.v.x - frame.u.x * frame.v.z,
                    frame.u.x * frame.v.y - frame.u.y * frame.v.x,
                );
                let dot = |a: Vector3, b: Vector3| a.x * b.x + a.y * b.y + a.z * b.z;
                let off_plane = dot(relative, normal).abs() + dot(*axis_direction, normal).abs();
                if off_plane > 1.0e-9 {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "The revolve axis must lie in the sketch plane",
                    ));
                }
                let start = Point2::new(dot(relative, frame.u), dot(relative, frame.v));
                let end = Point2::new(
                    start.x + dot(*axis_direction, frame.u),
                    start.y + dot(*axis_direction, frame.v),
                );
                Ok(KernelCommand::RevolvePlanarProfile {
                    frame,
                    profile,
                    axis: PlanarAxis2 { start, end },
                    angle: RevolveAngle::FullTurn,
                })
            }
            ApiCommand::BooleanUnion { .. }
            | ApiCommand::BooleanDifference { .. }
            | ApiCommand::BooleanIntersection { .. } => unreachable!("handled in execute()"),
        }
    }

    /// Resolves every edge selector, set selectors included, into one
    /// deduplicated edge list and the kernel command that finishes it.
    fn edge_finish(
        &self,
        edges: &[crate::selectors::EntitySelector],
        kind: EdgeFinishKind,
        distance: f64,
    ) -> Result<KernelCommand, ApiError> {
        let mut resolved = Vec::new();
        for selector in edges {
            for edge in resolve_selector_set(
                selector,
                &self.snapshot,
                &self.step_order,
                &self.step_reports,
            )? {
                if !resolved.contains(&edge) {
                    resolved.push(edge);
                }
            }
        }
        match resolved.as_slice() {
            [] => Err(ApiError::new(
                ApiErrorCode::SelectorNotFound,
                "No edge was selected for the finish",
            )),
            [single] => Ok(KernelCommand::FinishEdge {
                target_edge: *single,
                kind,
                distance,
            }),
            _ => Ok(KernelCommand::FinishEdges {
                target_edges: resolved,
                kind,
                distance,
            }),
        }
    }

    fn build_sketch_profile(
        &self,
        sketch: &crate::commands::StepLabel,
    ) -> Result<(PlanarFrame3, PlanarProfile2), ApiError> {
        let sketch_entry = self
            .journal
            .entries
            .iter()
            .find(|e| e.label == sketch.0)
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!("Referenced sketch \"{}\" not found in journal", sketch.0),
                )
            })?;

        match &sketch_entry.command {
            ApiCommand::Sketch { on, entities, .. } => {
                let frame = match on {
                    SketchPlane::XY => PlanarFrame3 {
                        origin: Point3::new(0.0, 0.0, 0.0),
                        u: Vector3::new(1.0, 0.0, 0.0),
                        v: Vector3::new(0.0, 1.0, 0.0),
                    },
                    SketchPlane::XZ => PlanarFrame3 {
                        origin: Point3::new(0.0, 0.0, 0.0),
                        u: Vector3::new(1.0, 0.0, 0.0),
                        v: Vector3::new(0.0, 0.0, 1.0),
                    },
                    SketchPlane::YZ => PlanarFrame3 {
                        origin: Point3::new(0.0, 0.0, 0.0),
                        u: Vector3::new(0.0, 1.0, 0.0),
                        v: Vector3::new(0.0, 0.0, 1.0),
                    },
                    SketchPlane::OnFace(face_sel) => {
                        let face_ref = resolve_selector(
                            face_sel,
                            &self.snapshot,
                            &self.step_order,
                            &self.step_reports,
                        )?;
                        let support = NativeKernel::planar_face_support(&self.snapshot, face_ref)
                            .map_err(ApiError::from)?;
                        support.frame
                    }
                };

                let loops = sketch_loops(entities)?;
                let profile = nest_loops(loops)?;
                Ok((frame, profile))
            }
            _ => Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                "Target step is not a Sketch",
            )),
        }
    }

    /// The face a sketch was drawn on, when it was drawn on one.
    fn sketch_face(
        &self,
        sketch: &crate::commands::StepLabel,
    ) -> Result<Option<artificer_protocol::EntityRef>, ApiError> {
        let entry = self
            .journal
            .entries
            .iter()
            .find(|entry| entry.label == sketch.0)
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!("Referenced sketch \"{}\" not found in journal", sketch.0),
                )
            })?;
        match &entry.command {
            ApiCommand::Sketch {
                on: SketchPlane::OnFace(face),
                ..
            } => resolve_selector(face, &self.snapshot, &self.step_order, &self.step_reports)
                .map(Some),
            ApiCommand::Sketch { .. } => Ok(None),
            _ => Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                "Target step is not a Sketch",
            )),
        }
    }

    #[must_use]
    pub fn query(&self) -> QueryHandle<'_> {
        QueryHandle::new(self)
    }

    pub fn snapshot(&self, options: SnapshotOptions) -> Result<SnapshotOutput, ApiError> {
        let last_report = self
            .step_order
            .last()
            .and_then(|label| self.step_reports.get(label));

        let mut highlighted = BTreeSet::new();
        for sel in &options.highlight {
            if let Ok(entity_ref) =
                resolve_selector(sel, &self.snapshot, &self.step_order, &self.step_reports)
            {
                highlighted.insert(entity_ref);
            }
        }

        render_snapshot(&self.snapshot, &options, last_report, &highlighted)
    }

    pub fn undo(&mut self) -> Result<(), ApiError> {
        if let Some((prev_snapshot, entry, _)) = self.undo_stack.pop() {
            self.redo_stack.push(entry);
            self.snapshot = prev_snapshot;
            if let Some(last_label) = self.step_order.pop() {
                self.step_reports.remove(&last_label);
                self.step_snapshots.remove(&last_label);
            }
            self.journal.entries.pop();
            Ok(())
        } else {
            Err(ApiError::new(ApiErrorCode::SessionError, "Nothing to undo"))
        }
    }

    pub fn redo(&mut self) -> Result<(), ApiError> {
        if let Some(entry) = self.redo_stack.pop() {
            self.execute(entry.command, &CancellationToken::default())?;
            Ok(())
        } else {
            Err(ApiError::new(ApiErrorCode::SessionError, "Nothing to redo"))
        }
    }

    pub fn export_journal(&self) -> Result<String, ApiError> {
        self.journal.to_json().map_err(ApiError::from)
    }

    pub fn from_journal(json: &str) -> Result<Self, ApiError> {
        let journal = Journal::from_json(json)?;
        if journal.schema_version != crate::journal::JOURNAL_SCHEMA_VERSION {
            return Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                format!(
                    "Journal schema version {} is not supported; this build reads version {}",
                    journal.schema_version,
                    crate::journal::JOURNAL_SCHEMA_VERSION
                ),
            ));
        }
        let mut session = Self::new();
        let token = CancellationToken::default();
        for entry in journal.entries {
            session.execute(entry.command, &token)?;
        }
        Ok(session)
    }
}

/// The closed loops a sketch's entities form. Circles and rectangles are
/// loops of their own; lines and arcs are chained end to end, in either
/// direction, until every one has been used and every chain has closed.
fn sketch_loops(entities: &[SketchEntity]) -> Result<Vec<Vec<PlanarCurve2>>, ApiError> {
    const JOIN: f64 = 1.0e-9;
    let mut loops = Vec::new();
    let mut open = Vec::new();
    for entity in entities {
        match entity {
            SketchEntity::Circle { center, radius } => loops.push(vec![PlanarCurve2::Circle {
                center: *center,
                radius: *radius,
                direction: ArcDirection::CounterClockwise,
            }]),
            SketchEntity::Rectangle {
                origin,
                width,
                height,
            } => {
                let corners = [
                    *origin,
                    Point2::new(origin.x + width, origin.y),
                    Point2::new(origin.x + width, origin.y + height),
                    Point2::new(origin.x, origin.y + height),
                ];
                loops.push(
                    (0..4)
                        .map(|index| PlanarCurve2::Line {
                            start: corners[index],
                            end: corners[(index + 1) % 4],
                        })
                        .collect(),
                );
            }
            SketchEntity::Line { start, end } => open.push(PlanarCurve2::Line {
                start: *start,
                end: *end,
            }),
            SketchEntity::Arc {
                center,
                radius,
                start_angle,
                end_angle,
            } => open.push(PlanarCurve2::CircularArc {
                center: *center,
                start: Point2::new(
                    center.x + radius * start_angle.cos(),
                    center.y + radius * start_angle.sin(),
                ),
                end: Point2::new(
                    center.x + radius * end_angle.cos(),
                    center.y + radius * end_angle.sin(),
                ),
                direction: ArcDirection::CounterClockwise,
            }),
        }
    }

    let endpoints = |curve: &PlanarCurve2| -> (Point2, Point2) {
        match curve {
            PlanarCurve2::Line { start, end } | PlanarCurve2::CircularArc { start, end, .. } => {
                (*start, *end)
            }
            PlanarCurve2::Circle { center, .. } => (*center, *center),
            PlanarCurve2::Bspline { control_points, .. } => (
                control_points.first().copied().unwrap_or_default(),
                control_points.last().copied().unwrap_or_default(),
            ),
        }
    };
    let near = |a: Point2, b: Point2| (a.x - b.x).hypot(a.y - b.y) <= JOIN;
    let reversed = |curve: &PlanarCurve2| -> PlanarCurve2 {
        match curve {
            PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
                start: *end,
                end: *start,
            },
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => PlanarCurve2::CircularArc {
                center: *center,
                start: *end,
                end: *start,
                direction: match direction {
                    ArcDirection::CounterClockwise => ArcDirection::Clockwise,
                    ArcDirection::Clockwise => ArcDirection::CounterClockwise,
                },
            },
            other => other.clone(),
        }
    };

    while let Some(first) = open.pop() {
        let (loop_start, mut cursor) = endpoints(&first);
        let mut chain = vec![first];
        while !near(cursor, loop_start) {
            let next = open.iter().position(|candidate| {
                let (start, end) = endpoints(candidate);
                near(start, cursor) || near(end, cursor)
            });
            let Some(index) = next else {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    format!(
                        "The sketch has an open chain ending at ({:.6}, {:.6}); every line and arc must join into a closed loop",
                        cursor.x, cursor.y
                    ),
                ));
            };
            let candidate = open.remove(index);
            let (start, _) = endpoints(&candidate);
            let oriented = if near(start, cursor) {
                candidate
            } else {
                reversed(&candidate)
            };
            cursor = endpoints(&oriented).1;
            chain.push(oriented);
        }
        loops.push(chain);
    }
    if loops.is_empty() {
        return Err(ApiError::new(
            ApiErrorCode::InvalidInput,
            "The sketch has no closed loop to extrude",
        ));
    }
    Ok(loops)
}

/// A polygon that follows a loop closely enough to decide containment.
fn loop_polygon(curves: &[PlanarCurve2]) -> Vec<Point2> {
    const ARC_SAMPLES: usize = 24;
    let mut polygon = Vec::new();
    for curve in curves {
        match curve {
            PlanarCurve2::Line { start, .. } => polygon.push(*start),
            PlanarCurve2::Circle { center, radius, .. } => {
                for index in 0..ARC_SAMPLES {
                    let angle = std::f64::consts::TAU * index as f64 / ARC_SAMPLES as f64;
                    polygon.push(Point2::new(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    ));
                }
            }
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let radius = (start.x - center.x).hypot(start.y - center.y);
                let from = (start.y - center.y).atan2(start.x - center.x);
                let to = (end.y - center.y).atan2(end.x - center.x);
                let sweep = match direction {
                    ArcDirection::CounterClockwise => (to - from).rem_euclid(std::f64::consts::TAU),
                    ArcDirection::Clockwise => -((from - to).rem_euclid(std::f64::consts::TAU)),
                };
                for index in 0..ARC_SAMPLES {
                    let angle = from + sweep * index as f64 / ARC_SAMPLES as f64;
                    polygon.push(Point2::new(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    ));
                }
            }
            PlanarCurve2::Bspline { control_points, .. } => {
                polygon.extend(control_points.iter().copied());
            }
        }
    }
    polygon
}

fn polygon_contains(polygon: &[Point2], point: Point2) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let start = polygon[index];
        let end = polygon[(index + 1) % polygon.len()];
        if (start.y > point.y) != (end.y > point.y)
            && point.x < (end.x - start.x) * (point.y - start.y) / (end.y - start.y) + start.x
        {
            inside = !inside;
        }
    }
    inside
}

/// Nests loops into regions: a loop inside no other loop is an outer
/// boundary, a loop inside exactly one is that region's hole. Deeper
/// nesting (an island inside a hole) is refused rather than guessed.
fn nest_loops(loops: Vec<Vec<PlanarCurve2>>) -> Result<PlanarProfile2, ApiError> {
    let polygons = loops
        .iter()
        .map(|curves| loop_polygon(curves))
        .collect::<Vec<_>>();
    let mut parents = vec![Vec::new(); loops.len()];
    for (index, polygon) in polygons.iter().enumerate() {
        for (other, container) in polygons.iter().enumerate() {
            if other != index
                && polygon
                    .iter()
                    .all(|point| polygon_contains(container, *point))
            {
                parents[index].push(other);
            }
        }
    }
    let mut regions = Vec::new();
    let mut outer_index = BTreeMap::new();
    for (index, parents) in parents.iter().enumerate() {
        if parents.is_empty() {
            outer_index.insert(index, regions.len());
            regions.push(PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: loops[index].clone(),
                },
                holes: Vec::new(),
            });
        }
    }
    for (index, parents) in parents.iter().enumerate() {
        match parents.as_slice() {
            [] => {}
            [parent] => {
                let region = outer_index.get(parent).ok_or_else(|| {
                    ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "A sketch loop lies inside another hole; islands are not supported",
                    )
                })?;
                regions[*region].holes.push(PlanarLoop2 {
                    curves: loops[index].clone(),
                });
            }
            _ => {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    "A sketch loop is nested more than one level deep; islands inside holes are not supported",
                ));
            }
        }
    }
    Ok(PlanarProfile2 { regions })
}

/// Keeps only the requested regions, by index in nesting order; an empty
/// request keeps them all.
fn select_regions(profile: PlanarProfile2, regions: &[u32]) -> Result<PlanarProfile2, ApiError> {
    if regions.is_empty() {
        return Ok(profile);
    }
    let count = profile.regions.len();
    let mut selected = Vec::new();
    for index in regions {
        let index = *index as usize;
        let region = profile.regions.get(index).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::InvalidInput,
                format!("Region {index} does not exist; the sketch has {count} regions"),
            )
        })?;
        selected.push(region.clone());
    }
    Ok(PlanarProfile2 { regions: selected })
}

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

use crate::commands::{ApiCommand, SketchEntity, SketchPlane};
use crate::debug::{ApiError, ApiErrorCode, CommandResult, EntityInfo};
use crate::journal::{Journal, JournalEntry};
use crate::query::QueryHandle;
use crate::selectors::resolve_selector;
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
            .unwrap_or(self.snapshot.id());
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

                let u = if axis_unit.x.abs() < 0.9 {
                    Vector3::new(1.0, 0.0, 0.0)
                } else {
                    Vector3::new(0.0, 1.0, 0.0)
                };
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
                Ok(KernelCommand::DrillHole {
                    target_face,
                    frame: support.frame,
                    center: *center,
                    diameter: *diameter,
                    depth: *depth,
                })
            }
            ApiCommand::Fillet { edges, radius, .. } => {
                let mut resolved_edges = Vec::new();
                for edge_sel in edges {
                    resolved_edges.push(resolve_selector(
                        edge_sel,
                        &self.snapshot,
                        &self.step_order,
                        &self.step_reports,
                    )?);
                }
                if resolved_edges.len() == 1 {
                    Ok(KernelCommand::FinishEdge {
                        target_edge: resolved_edges[0],
                        kind: EdgeFinishKind::Fillet,
                        distance: *radius,
                    })
                } else {
                    Ok(KernelCommand::FinishEdges {
                        target_edges: resolved_edges,
                        kind: EdgeFinishKind::Fillet,
                        distance: *radius,
                    })
                }
            }
            ApiCommand::Chamfer {
                edges, distance, ..
            } => {
                let mut resolved_edges = Vec::new();
                for edge_sel in edges {
                    resolved_edges.push(resolve_selector(
                        edge_sel,
                        &self.snapshot,
                        &self.step_order,
                        &self.step_reports,
                    )?);
                }
                if resolved_edges.len() == 1 {
                    Ok(KernelCommand::FinishEdge {
                        target_edge: resolved_edges[0],
                        kind: EdgeFinishKind::Chamfer,
                        distance: *distance,
                    })
                } else {
                    Ok(KernelCommand::FinishEdges {
                        target_edges: resolved_edges,
                        kind: EdgeFinishKind::Chamfer,
                        distance: *distance,
                    })
                }
            }
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
            ApiCommand::Sketch { .. } => Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                "Sketch step must be followed by an Extrude or Revolve feature",
            )),
            ApiCommand::Extrude {
                sketch, distance, ..
            } => {
                let (frame, profile) = self.build_sketch_profile(sketch)?;
                Ok(KernelCommand::ExtrudePlanarProfile {
                    frame,
                    profile,
                    distance: *distance,
                })
            }
            ApiCommand::Revolve { sketch, .. } => {
                let (frame, profile) = self.build_sketch_profile(sketch)?;
                let axis = PlanarAxis2 {
                    start: Point2::new(0.0, 0.0),
                    end: Point2::new(0.0, 1.0),
                };
                Ok(KernelCommand::RevolvePlanarProfile {
                    frame,
                    profile,
                    axis,
                    angle: RevolveAngle::FullTurn,
                })
            }
            ApiCommand::BooleanUnion { .. }
            | ApiCommand::BooleanDifference { .. }
            | ApiCommand::BooleanIntersection { .. } => unreachable!("handled in execute()"),
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

                let mut curves = Vec::new();
                for ent in entities {
                    match ent {
                        SketchEntity::Line { start, end } => {
                            curves.push(PlanarCurve2::Line {
                                start: *start,
                                end: *end,
                            });
                        }
                        SketchEntity::Rectangle {
                            origin,
                            width,
                            height,
                        } => {
                            let p0 = *origin;
                            let p1 = Point2::new(origin.x + width, origin.y);
                            let p2 = Point2::new(origin.x + width, origin.y + height);
                            let p3 = Point2::new(origin.x, origin.y + height);
                            curves.push(PlanarCurve2::Line { start: p0, end: p1 });
                            curves.push(PlanarCurve2::Line { start: p1, end: p2 });
                            curves.push(PlanarCurve2::Line { start: p2, end: p3 });
                            curves.push(PlanarCurve2::Line { start: p3, end: p0 });
                        }
                        SketchEntity::Circle { center, radius } => {
                            curves.push(PlanarCurve2::Circle {
                                center: *center,
                                radius: *radius,
                                direction: ArcDirection::CounterClockwise,
                            });
                        }
                        SketchEntity::Arc {
                            center,
                            radius,
                            start_angle,
                            end_angle,
                        } => {
                            let start_x = center.x + radius * start_angle.cos();
                            let start_y = center.y + radius * start_angle.sin();
                            let end_x = center.x + radius * end_angle.cos();
                            let end_y = center.y + radius * end_angle.sin();
                            curves.push(PlanarCurve2::CircularArc {
                                center: *center,
                                start: Point2::new(start_x, start_y),
                                end: Point2::new(end_x, end_y),
                                direction: ArcDirection::CounterClockwise,
                            });
                        }
                    }
                }

                let profile = PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: PlanarLoop2 { curves },
                        holes: Vec::new(),
                    }],
                };

                Ok((frame, profile))
            }
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
        let mut session = Self::new();
        let token = CancellationToken::default();
        for entry in journal.entries {
            session.execute(entry.command, &token)?;
        }
        Ok(session)
    }
}

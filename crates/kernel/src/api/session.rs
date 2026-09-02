//! Live kernel session state, command execution, and transaction tracking.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EdgeFinishKind,
    ExecuteRequest, KernelCommand, OperationReport, PlanarAxis2, PlanarCurve2, PlanarFrame3,
    PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId,
    RevolveAngle, SnapshotId, Tier, Vector3,
};

use artificer_protocol::FaceExtrusionOperation;

use crate::api::commands::{
    ApiCommand, ExtrudeOp, PatternPlacement, SketchEntity, SketchPlane, StepLabel,
};
use crate::api::debug::{ApiError, ApiErrorCode, CommandResult, EntityInfo};
use crate::api::journal::{Journal, JournalEntry};
use crate::api::query::QueryHandle;
use crate::api::selectors::{EntitySelector, resolve_selector, resolve_selector_set};
use crate::api::snapshot::{SnapshotOptions, SnapshotOutput, render_snapshot};

/// The rung a feature pattern step reports. The instances under it carry
/// the rungs that built each of them.
pub const PATTERN_RUNG: &str = "pattern/replay";

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
    /// How long each step took to execute, by label.
    pub step_elapsed_ms: BTreeMap<String, u64>,
    /// The command kind of every step, by label, instance steps of a
    /// pattern included.
    pub step_kinds: BTreeMap<String, String>,
    pub undo_stack: Vec<(Snapshot, JournalEntry, Option<OperationReport>)>,
    pub redo_stack: Vec<JournalEntry>,
    /// The resolved parameters of the script this session ran, when it ran
    /// one; the session report carries them so a result can be reproduced.
    pub parameters: BTreeMap<String, f64>,
    /// The names a script gave to faces and edges with `let name =
    /// <selector>`, resolved against the current body by the report.
    pub names: Vec<(String, EntitySelector)>,
    /// The sketches a feature pattern drew for its instances, by label.
    /// They are steps but not journal entries: the pattern's own entry
    /// stands for them, and they go when it is undone.
    pub pattern_sketches: BTreeMap<String, ApiCommand>,
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
            step_elapsed_ms: BTreeMap::new(),
            step_kinds: BTreeMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            parameters: BTreeMap::new(),
            names: Vec::new(),
            pattern_sketches: BTreeMap::new(),
        }
    }

    pub fn execute(
        &mut self,
        command: ApiCommand,
        token: &CancellationToken,
    ) -> Result<CommandResult, ApiError> {
        self.execute_recorded(command, token, true)
    }

    /// Executes one command. With `record`, the step is journaled and can
    /// be undone; without it, the step is an instance of a feature pattern,
    /// committed under the pattern's journal entry.
    fn execute_recorded(
        &mut self,
        command: ApiCommand,
        token: &CancellationToken,
        record: bool,
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
            return Ok(self.record_sketch(command, start_time, record));
        }
        if let ApiCommand::FeaturePattern {
            step, placement, ..
        } = &command
        {
            let (step, placement) = (step.clone(), placement.clone());
            return self.execute_feature_pattern(
                command, step_label, &step, &placement, token, start_time, record,
            );
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
                // A command that starts a body of its own runs from an empty
                // snapshot rather than the current one: the kernel's
                // constructors replace whatever they are given, so building
                // from the current snapshot would only refuse a second body.
                // Every body therefore lives in its own step chain, and the
                // Booleans join those chains by step label.
                let empty;
                let input = if Self::starts_new_body(&command) {
                    empty = NativeKernel::empty();
                    &empty
                } else {
                    &self.snapshot
                };
                let request = ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!("session::{step_label}")),
                    expected_snapshot: input.id(),
                    precision: self.precision,
                    command: kernel_cmd,
                };
                NativeKernel::execute(input, &request, token).map_err(ApiError::from)?
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

        let tier = outcome.report.tier();
        let result = CommandResult {
            success: outcome.report.validation.valid,
            step_label: step_label.clone(),
            snapshot_id: outcome.snapshot.id(),
            topology: outcome.snapshot.counts(),
            bounds: outcome.snapshot.measures().bounds,
            entities: entity_map,
            diagnostics: outcome.report.validation.diagnostics.clone(),
            warnings: outcome.report.warnings.clone(),
            rung: outcome.report.rung.clone(),
            tier,
            elapsed_ms,
            summary: format!(
                "Step \"{}\" committed snapshot {}. {}{}",
                step_label,
                outcome.snapshot.id(),
                outcome.snapshot.counts(),
                if tier == Tier::Approximate {
                    " Approximate: the faceted tier built this step."
                } else {
                    ""
                }
            ),
        };

        // Record undo state, cache snapshot, and journal entry
        if record {
            let entry = JournalEntry::new(command.clone());
            self.undo_stack.push((
                self.snapshot.clone(),
                entry.clone(),
                Some(outcome.report.clone()),
            ));
            self.redo_stack.clear();
            self.journal.push(entry);
        }

        self.step_order.push(step_label.clone());
        self.step_kinds
            .insert(step_label.clone(), command.kind().to_owned());
        self.step_reports.insert(step_label.clone(), outcome.report);
        self.step_elapsed_ms.insert(step_label.clone(), elapsed_ms);
        self.step_snapshots
            .insert(step_label, outcome.snapshot.id());
        self.snapshot_cache
            .insert(outcome.snapshot.id(), outcome.snapshot.clone());
        self.snapshot = outcome.snapshot;

        Ok(result)
    }

    /// Replays the source feature at every placement of the pattern,
    /// committing each instance as `<label>/<n>` under one journal entry.
    #[allow(clippy::too_many_arguments)]
    fn execute_feature_pattern(
        &mut self,
        command: ApiCommand,
        label: String,
        source: &StepLabel,
        placement: &PatternPlacement,
        token: &CancellationToken,
        start_time: Instant,
        record: bool,
    ) -> Result<CommandResult, ApiError> {
        let source_command = self
            .journal
            .entries
            .iter()
            .find(|entry| entry.label == source.0)
            .map(|entry| entry.command.clone())
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!(
                        "Step \"{}\" is not in the session, so it cannot be patterned",
                        source.0
                    ),
                )
            })?;
        let instances = self.pattern_instances(&label, &source_command, placement)?;
        let before = self.snapshot.clone();
        let mut tier = Tier::Exact;
        let mut warnings = Vec::new();
        let mut entities = BTreeMap::new();
        for instance in instances {
            let result = match self.execute_recorded(instance, token, false) {
                Ok(result) => result,
                Err(error) => {
                    // A pattern commits whole or not at all: the instances
                    // that did build are discarded with the failed one.
                    self.snapshot = before;
                    self.discard_steps_under(&label);
                    return Err(error);
                }
            };
            tier = tier.combine(result.tier);
            warnings.extend(result.warnings);
            for (role, info) in result.entities {
                entities.insert(format!("{}.{role}", result.step_label), info);
            }
        }
        let elapsed_ms = start_time.elapsed().as_millis() as u64;
        if record {
            let entry = JournalEntry::new(command);
            self.undo_stack.push((before, entry.clone(), None));
            self.redo_stack.clear();
            self.journal.push(entry);
        }
        // The pattern step itself: the last instance's report stands for it,
        // so `pattern.face("role")` reaches the last instance. Its rung is
        // the pattern's own; the instances carry the rungs that built them.
        let last_label = self.step_order.last().cloned();
        if let Some(mut report) = last_label.and_then(|last| self.step_reports.get(&last).cloned())
        {
            report.rung = Some(PATTERN_RUNG.to_owned());
            self.step_reports.insert(label.clone(), report);
        }
        self.step_order.push(label.clone());
        self.step_kinds
            .insert(label.clone(), "feature_pattern".to_owned());
        self.step_elapsed_ms.insert(label.clone(), elapsed_ms);
        self.step_snapshots
            .insert(label.clone(), self.snapshot.id());
        Ok(CommandResult {
            success: true,
            step_label: label.clone(),
            snapshot_id: self.snapshot.id(),
            topology: self.snapshot.counts(),
            bounds: self.snapshot.measures().bounds,
            entities,
            diagnostics: Vec::new(),
            warnings,
            rung: Some(PATTERN_RUNG.to_owned()),
            tier,
            elapsed_ms,
            summary: format!(
                "Pattern \"{label}\" replayed \"{}\" as {} more instances; snapshot {}. {}",
                source.0,
                placement.count().saturating_sub(1),
                self.snapshot.id(),
                self.snapshot.counts()
            ),
        })
    }

    /// The commands that make one pattern's instances: the source feature
    /// with its face-frame geometry moved to each placement.
    fn pattern_instances(
        &self,
        label: &str,
        source: &ApiCommand,
        placement: &PatternPlacement,
    ) -> Result<Vec<ApiCommand>, ApiError> {
        let count = placement.count();
        if !(2..=128).contains(&count) {
            return Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                "A pattern has between 2 and 128 instances, the original included",
            ));
        }
        // The face the feature sits on and its frame, which the placements
        // are expressed in.
        let (face, sketch_source) = match source {
            ApiCommand::DrillHole { face, .. } => (face.clone(), None),
            ApiCommand::Extrude {
                sketch, operation, ..
            } if *operation != ExtrudeOp::New => {
                let entry = self
                    .journal
                    .entries
                    .iter()
                    .find(|entry| entry.label == sketch.0)
                    .ok_or_else(|| {
                        ApiError::new(
                            ApiErrorCode::SelectorNotFound,
                            format!("Sketch \"{}\" is not in the session", sketch.0),
                        )
                    })?;
                match &entry.command {
                    ApiCommand::Sketch {
                        on: SketchPlane::OnFace { face },
                        entities,
                        ..
                    } => (face.clone(), Some(entities.clone())),
                    _ => {
                        return Err(ApiError::new(
                            ApiErrorCode::InvalidInput,
                            "A pattern replays an extrusion from a sketch drawn on a face; this sketch is on a world plane",
                        ));
                    }
                }
            }
            other => {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    format!(
                        "A pattern replays a drilled hole or an add/cut extrusion from a face sketch; \"{}\" is a {}",
                        other.label(),
                        other.kind()
                    ),
                ));
            }
        };
        // The face the source was built on, followed through every step
        // since by history: a selector like `faces(">Z")` would land on a
        // boss the source itself added. The source's own selector is the
        // fallback when the step left no such record.
        let host = EntitySelector::ByHistory {
            from_step: StepLabel(source.label().to_owned()),
            kind: artificer_protocol::EntityKind::Face,
            role: "face_extrude.target_face_patch".to_owned(),
            ordinal: None,
        };
        let by_history =
            resolve_selector(&host, &self.snapshot, &self.step_order, &self.step_reports).and_then(
                |face_ref| {
                    NativeKernel::planar_face_support(&self.snapshot, face_ref)
                        .map_err(ApiError::from)
                },
            );
        let (face, support) = match by_history {
            Ok(support) => (host, support),
            Err(_) => {
                let face_ref =
                    resolve_selector(&face, &self.snapshot, &self.step_order, &self.step_reports)?;
                let support = NativeKernel::planar_face_support(&self.snapshot, face_ref)
                    .map_err(ApiError::from)?;
                (face, support)
            }
        };
        let frame = support.frame;
        let dot = |a: Vector3, b: Vector3| a.x * b.x + a.y * b.y + a.z * b.z;
        let normal = Vector3::new(
            frame.u.y * frame.v.z - frame.u.z * frame.v.y,
            frame.u.z * frame.v.x - frame.u.x * frame.v.z,
            frame.u.x * frame.v.y - frame.u.y * frame.v.x,
        );
        let length = |v: Vector3| dot(v, v).sqrt();

        // Each instance's map from the face frame to itself.
        let maps: Vec<Box<dyn Fn(Point2) -> Point2>> = match placement {
            PatternPlacement::Linear {
                direction,
                spacing,
                count,
            } => {
                let size = length(*direction);
                if !size.is_finite() || size <= 1.0e-12 || !spacing.is_finite() || *spacing <= 0.0 {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "A linear pattern needs a direction and a positive spacing",
                    ));
                }
                if dot(*direction, normal).abs() > 1.0e-9 * size * length(normal) {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "A linear pattern's direction must lie in the feature's face",
                    ));
                }
                let step = Point2::new(
                    dot(*direction, frame.u) / size * spacing,
                    dot(*direction, frame.v) / size * spacing,
                );
                (1..*count)
                    .map(|k| {
                        let k = f64::from(k);
                        Box::new(move |p: Point2| Point2::new(p.x + k * step.x, p.y + k * step.y))
                            as Box<dyn Fn(Point2) -> Point2>
                    })
                    .collect()
            }
            PatternPlacement::Circular {
                axis_origin,
                axis_direction,
                count,
                angle_step_degrees,
            } => {
                let size = length(*axis_direction);
                if !size.is_finite() || size <= 1.0e-12 {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "A circular pattern needs an axis direction",
                    ));
                }
                let cross = Vector3::new(
                    axis_direction.y * normal.z - axis_direction.z * normal.y,
                    axis_direction.z * normal.x - axis_direction.x * normal.z,
                    axis_direction.x * normal.y - axis_direction.y * normal.x,
                );
                if length(cross) > 1.0e-9 * size * length(normal) {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "A circular pattern's axis must be normal to the feature's face",
                    ));
                }
                let relative = Vector3::new(
                    axis_origin.x - frame.origin.x,
                    axis_origin.y - frame.origin.y,
                    axis_origin.z - frame.origin.z,
                );
                let centre = Point2::new(dot(relative, frame.u), dot(relative, frame.v));
                let step = if *angle_step_degrees == 0.0 {
                    360.0 / f64::from(*count)
                } else {
                    *angle_step_degrees
                };
                // A positive angle turns counter-clockwise about the axis
                // direction; in the face frame that is the sign of the axis
                // against the face normal.
                let sense = if dot(*axis_direction, normal) >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                (1..*count)
                    .map(|k| {
                        let angle = (f64::from(k) * step * sense).to_radians();
                        let (sin, cos) = angle.sin_cos();
                        Box::new(move |p: Point2| {
                            let dx = p.x - centre.x;
                            let dy = p.y - centre.y;
                            Point2::new(
                                centre.x + dx * cos - dy * sin,
                                centre.y + dx * sin + dy * cos,
                            )
                        }) as Box<dyn Fn(Point2) -> Point2>
                    })
                    .collect()
            }
        };
        let rotation_degrees = |k: usize| -> f64 {
            match placement {
                PatternPlacement::Linear { .. } => 0.0,
                PatternPlacement::Circular {
                    count,
                    angle_step_degrees,
                    axis_direction,
                    ..
                } => {
                    let step = if *angle_step_degrees == 0.0 {
                        360.0 / f64::from(*count)
                    } else {
                        *angle_step_degrees
                    };
                    let sense = if dot(*axis_direction, normal) >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    };
                    (k as f64) * step * sense
                }
            }
        };

        // A placement that carries the feature off its face is refused
        // here, by name, rather than left to cut nothing.
        let leaves_face = |footprint: &[Point2]| {
            footprint
                .iter()
                .any(|point| !point_inside_polygon(*point, &support.boundary))
        };
        let off_face = |k: usize| {
            ApiError::new(
                ApiErrorCode::InvalidInput,
                format!(
                    "Instance {k} of pattern \"{label}\" leaves the face the feature is on; move the placement or lower the count"
                ),
            )
        };

        let mut instances = Vec::new();
        for (index, map) in maps.iter().enumerate() {
            let k = index + 1;
            let instance_label = format!("{label}/{k}");
            match source {
                ApiCommand::DrillHole {
                    center,
                    diameter,
                    depth,
                    ..
                } => {
                    let centre = map(*center);
                    let footprint = SketchEntity::Circle {
                        center: centre,
                        radius: diameter / 2.0,
                    };
                    if leaves_face(&footprint_points(std::slice::from_ref(&footprint))) {
                        return Err(off_face(k));
                    }
                    instances.push(ApiCommand::DrillHole {
                        label: instance_label,
                        face: face.clone(),
                        center: centre,
                        diameter: *diameter,
                        depth: *depth,
                    });
                }
                ApiCommand::Extrude {
                    regions,
                    distance,
                    operation,
                    draft_degrees,
                    ..
                } => {
                    let entities = sketch_source
                        .as_ref()
                        .map(|entities| moved_entities(entities, map, rotation_degrees(k)))
                        .unwrap_or_default();
                    if leaves_face(&footprint_points(&entities)) {
                        return Err(off_face(k));
                    }
                    let sketch_label = format!("{instance_label}/sketch");
                    instances.push(ApiCommand::Sketch {
                        label: sketch_label.clone(),
                        on: SketchPlane::OnFace { face: face.clone() },
                        entities,
                        constraints: Vec::new(),
                    });
                    instances.push(ApiCommand::Extrude {
                        label: instance_label,
                        sketch: StepLabel(sketch_label),
                        regions: regions.clone(),
                        distance: *distance,
                        operation: *operation,
                        draft_degrees: *draft_degrees,
                    });
                }
                _ => unreachable!("checked above"),
            }
        }
        Ok(instances)
    }

    /// Whether a command builds a body of its own instead of editing the
    /// current one.
    fn starts_new_body(command: &ApiCommand) -> bool {
        match command {
            ApiCommand::MakeBox { .. } | ApiCommand::MakeCylinder { .. } => true,
            ApiCommand::Extrude { operation, .. } | ApiCommand::Revolve { operation, .. } => {
                *operation == ExtrudeOp::New
            }
            _ => false,
        }
    }

    /// A sketch is authoring intent rather than a kernel operation: it is
    /// journaled as a step of its own, leaves the snapshot untouched, and is
    /// consumed by the Extrude or Revolve that names it.
    fn record_sketch(
        &mut self,
        command: ApiCommand,
        start_time: Instant,
        record: bool,
    ) -> CommandResult {
        let step_label = command.label().to_owned();
        if record {
            let entry = JournalEntry::new(command.clone());
            self.undo_stack
                .push((self.snapshot.clone(), entry.clone(), None));
            self.redo_stack.clear();
            self.journal.push(entry);
        } else {
            self.pattern_sketches
                .insert(step_label.clone(), command.clone());
        }
        self.step_order.push(step_label.clone());
        self.step_kinds
            .insert(step_label.clone(), command.kind().to_owned());
        self.step_snapshots
            .insert(step_label.clone(), self.snapshot.id());
        let elapsed_ms = start_time.elapsed().as_millis() as u64;
        self.step_elapsed_ms.insert(step_label.clone(), elapsed_ms);
        CommandResult {
            success: true,
            step_label: step_label.clone(),
            snapshot_id: self.snapshot.id(),
            topology: self.snapshot.counts(),
            bounds: self.snapshot.measures().bounds,
            entities: BTreeMap::new(),
            diagnostics: Vec::new(),
            warnings: Vec::new(),
            rung: None,
            tier: Tier::Exact,
            elapsed_ms,
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
            ApiCommand::Shell { open, wall, .. } => {
                let open_faces = open
                    .iter()
                    .map(|face| {
                        resolve_selector(face, &self.snapshot, &self.step_order, &self.step_reports)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(KernelCommand::ShellSnapshot {
                    open_faces,
                    wall: *wall,
                })
            }
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
                draft_degrees,
                ..
            } => {
                let (frame, profile) = self.build_sketch_profile(sketch)?;
                let profile = select_regions(profile, regions)?;
                if !draft_degrees.is_finite() || draft_degrees.abs() >= 90.0 {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "A draft angle must be finite and less than 90 degrees",
                    ));
                }
                if *draft_degrees != 0.0 && *operation != ExtrudeOp::New {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        "Only a new-body extrusion can draft; add and cut extrusions build straight walls",
                    ));
                }
                match operation {
                    ExtrudeOp::New if *draft_degrees != 0.0 => {
                        Ok(KernelCommand::LoftPlanarProfileOffset {
                            frame,
                            profile,
                            distance: *distance,
                            offset: *distance * draft_degrees.to_radians().tan(),
                        })
                    }
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
            | ApiCommand::BooleanIntersection { .. }
            | ApiCommand::FeaturePattern { .. } => unreachable!("handled in execute()"),
        }
    }

    /// Resolves every edge selector, set selectors included, into one
    /// deduplicated edge list and the kernel command that finishes it.
    fn edge_finish(
        &self,
        edges: &[crate::api::selectors::EntitySelector],
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
        sketch: &crate::api::commands::StepLabel,
    ) -> Result<(PlanarFrame3, PlanarProfile2), ApiError> {
        match self.sketch_command(sketch)? {
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
                    SketchPlane::OnFace { face: face_sel } => {
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
        sketch: &crate::api::commands::StepLabel,
    ) -> Result<Option<artificer_protocol::EntityRef>, ApiError> {
        match self.sketch_command(sketch)? {
            ApiCommand::Sketch {
                on: SketchPlane::OnFace { face },
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
            self.snapshot = prev_snapshot;
            self.journal.entries.pop();
            self.discard_steps_under(&entry.label);
            self.redo_stack.push(entry);
            Ok(())
        } else {
            Err(ApiError::new(ApiErrorCode::SessionError, "Nothing to undo"))
        }
    }

    /// Forgets the newest steps that belong to `label`: the step itself and
    /// the instance steps a pattern committed under it as `<label>/<n>`,
    /// which are never journal entries of their own.
    fn discard_steps_under(&mut self, label: &str) {
        let prefix = format!("{label}/");
        while let Some(last) = self.step_order.last().cloned() {
            let owned = last == label
                || (last.starts_with(&prefix)
                    && !self.journal.entries.iter().any(|entry| entry.label == last));
            if !owned {
                break;
            }
            self.step_order.pop();
            self.step_reports.remove(&last);
            self.step_snapshots.remove(&last);
            self.step_elapsed_ms.remove(&last);
            self.step_kinds.remove(&last);
            self.pattern_sketches.remove(&last);
        }
    }

    /// The sketch command a step label names: a journal entry, or a sketch
    /// a pattern drew for one of its instances.
    fn sketch_command(&self, sketch: &StepLabel) -> Result<&ApiCommand, ApiError> {
        self.journal
            .entries
            .iter()
            .find(|entry| entry.label == sketch.0)
            .map(|entry| &entry.command)
            .or_else(|| self.pattern_sketches.get(&sketch.0))
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!("Referenced sketch \"{}\" not found in journal", sketch.0),
                )
            })
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
        if journal.schema_version != crate::api::journal::JOURNAL_SCHEMA_VERSION {
            return Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                format!(
                    "Journal schema version {} is not supported; this build reads version {}",
                    journal.schema_version,
                    crate::api::journal::JOURNAL_SCHEMA_VERSION
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

/// A sketch's entities moved by a pattern placement: every point through
/// `map`, arcs also turned by `rotation_degrees`, rectangles as four lines
/// when they turn.
fn moved_entities(
    entities: &[SketchEntity],
    map: &dyn Fn(Point2) -> Point2,
    rotation_degrees: f64,
) -> Vec<SketchEntity> {
    let turned = rotation_degrees != 0.0;
    let mut moved = Vec::new();
    for entity in entities {
        match entity {
            SketchEntity::Line { start, end } => moved.push(SketchEntity::Line {
                start: map(*start),
                end: map(*end),
            }),
            SketchEntity::Circle { center, radius } => moved.push(SketchEntity::Circle {
                center: map(*center),
                radius: *radius,
            }),
            SketchEntity::Arc {
                center,
                radius,
                start_angle,
                end_angle,
            } => moved.push(SketchEntity::Arc {
                center: map(*center),
                radius: *radius,
                start_angle: start_angle + rotation_degrees.to_radians(),
                end_angle: end_angle + rotation_degrees.to_radians(),
            }),
            SketchEntity::Rectangle {
                origin,
                width,
                height,
            } => {
                if turned {
                    let corners = [
                        *origin,
                        Point2::new(origin.x + width, origin.y),
                        Point2::new(origin.x + width, origin.y + height),
                        Point2::new(origin.x, origin.y + height),
                    ];
                    for index in 0..4 {
                        moved.push(SketchEntity::Line {
                            start: map(corners[index]),
                            end: map(corners[(index + 1) % 4]),
                        });
                    }
                } else {
                    moved.push(SketchEntity::Rectangle {
                        origin: map(*origin),
                        width: *width,
                        height: *height,
                    });
                }
            }
        }
    }
    moved
}

/// Points that bound what sketch entities cover: endpoints, corners, and
/// the axis extremes of circles and arcs. Every one inside a face means
/// the entities are, for the placements a pattern makes.
fn footprint_points(entities: &[SketchEntity]) -> Vec<Point2> {
    let mut points = Vec::new();
    for entity in entities {
        match entity {
            SketchEntity::Line { start, end } => points.extend([*start, *end]),
            SketchEntity::Circle { center, radius } => points.extend([
                Point2::new(center.x + radius, center.y),
                Point2::new(center.x - radius, center.y),
                Point2::new(center.x, center.y + radius),
                Point2::new(center.x, center.y - radius),
            ]),
            SketchEntity::Arc {
                center,
                radius,
                start_angle,
                end_angle,
            } => {
                let (start, end) = (start_angle.min(*end_angle), start_angle.max(*end_angle));
                let at = |angle: f64| {
                    Point2::new(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    )
                };
                points.extend([at(*start_angle), at(*end_angle)]);
                // The cardinal directions the arc passes through.
                let first = (start / std::f64::consts::FRAC_PI_2).ceil() as i64;
                let last = (end / std::f64::consts::FRAC_PI_2).floor() as i64;
                for quarter in first..=last {
                    points.push(at(quarter as f64 * std::f64::consts::FRAC_PI_2));
                }
            }
            SketchEntity::Rectangle {
                origin,
                width,
                height,
            } => points.extend([
                *origin,
                Point2::new(origin.x + width, origin.y),
                Point2::new(origin.x + width, origin.y + height),
                Point2::new(origin.x, origin.y + height),
            ]),
        }
    }
    points
}

/// Whether a point lies strictly inside a polygon, by ray parity; a point
/// on the boundary counts as outside.
fn point_inside_polygon(point: Point2, polygon: &[Point2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let epsilon = 1.0e-9;
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        let (dx, dy) = (current.x - previous.x, current.y - previous.y);
        // On the segment: outside.
        let cross = dx * (point.y - previous.y) - dy * (point.x - previous.x);
        let length = (dx * dx + dy * dy).sqrt();
        if length > 0.0 && cross.abs() <= epsilon * length {
            let along =
                ((point.x - previous.x) * dx + (point.y - previous.y) * dy) / (length * length);
            if (-epsilon..=1.0 + epsilon).contains(&along) {
                return false;
            }
        }
        if (previous.y > point.y) != (current.y > point.y) {
            let x = previous.x + (point.y - previous.y) * dx / dy;
            if point.x < x {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

/// The closed loops a sketch's entities form. Circles and rectangles are
/// loops of their own; lines and arcs are chained end to end, in either
/// direction, until every one has been used and every chain has closed.
/// Moves one end of a line or arc onto `point`, for exact chaining.
fn set_endpoint(curve: &mut PlanarCurve2, at_start: bool, point: Point2) {
    match curve {
        PlanarCurve2::Line { start, end } | PlanarCurve2::CircularArc { start, end, .. } => {
            if at_start {
                *start = point;
            } else {
                *end = point;
            }
        }
        PlanarCurve2::Circle { .. } | PlanarCurve2::Bspline { .. } => {}
    }
}

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
            let mut oriented = if near(start, cursor) {
                candidate
            } else {
                reversed(&candidate)
            };
            // The kernel wants the chain exact, and an arc's computed end
            // can miss the next line's start by rounding: the junction is
            // the point already on the chain.
            set_endpoint(&mut oriented, true, cursor);
            cursor = endpoints(&oriented).1;
            chain.push(oriented);
        }
        if chain.len() > 1
            && let Some(last) = chain.last_mut()
        {
            set_endpoint(last, false, loop_start);
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

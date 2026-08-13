//! Declarative cases, deterministic replay, and visual artifacts for the native kernel.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use artificer_kernel::{CancellationToken, DebugScene, FaceRole, NativeKernel, SnapshotMeasures};
use artificer_protocol::{
    Aabb3, CURRENT_PROTOCOL_VERSION, DiagnosticCode, EntityKind, EntityRef, ExecuteRequest,
    HistoryRelation, KernelCommand, KernelError, KernelErrorCode, OperationReport, Point3,
    PrecisionPolicy, ProtocolVersion, RequestId, SemanticDigest, SnapshotId, TopologyCounts,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const KERNEL_CASE_SCHEMA_VERSION: u32 = 1;
pub const COMMAND_JOURNAL_SCHEMA_VERSION: u32 = 0;
pub const FAILURE_BUNDLE_SCHEMA_VERSION: u32 = 1;

mod finite_f64 {
    use serde::{Deserialize, Deserializer, Serializer, de, ser};

    pub fn serialize<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if value.is_finite() {
            serializer.serialize_f64(*value)
        } else {
            Err(ser::Error::custom(
                "non-finite floating-point values are not serializable",
            ))
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        if value.is_finite() {
            Ok(value)
        } else {
            Err(de::Error::custom(
                "non-finite floating-point values are not deserializable",
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelCase {
    pub schema_version: u32,
    pub protocol_version: ProtocolVersion,
    pub case_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    pub precision: PrecisionPolicy,
    pub steps: Vec<CaseStep>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseStep {
    pub step_id: String,
    pub command: KernelCommand,
    /// Test-only late binding for the command's target entity.
    ///
    /// The case runner resolves this selector through prior operation history
    /// before execution. Journals retain only the resulting concrete protocol
    /// command, so replay never depends on case-layer reference semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_entity: Option<HistoryOutputReference>,
    pub expected: ExpectedOutcome,
}

/// Selects one entity introduced or carried by a prior case step.
///
/// This is intentionally a test-fixture reference rather than a product
/// persistent name. `from_step` identifies the operation report in which the
/// semantic role originated. If later successful steps changed the snapshot,
/// the runner follows their operation history until the entity belongs to the
/// current immutable snapshot. Deleted or one-to-many results fail closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryOutputReference {
    pub from_step: String,
    pub kind: EntityKind,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedOutcome {
    Success {
        topology: TopologyCounts,
        bounds: ExpectedAabb3,
        #[serde(default = "default_true")]
        validation_valid: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        semantic_digest: Option<SemanticDigest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        measures: Option<ExpectedMeasures>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history: Option<ExpectedHistory>,
    },
    Error {
        code: KernelErrorCode,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostic_codes: Vec<DiagnosticCode>,
    },
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedAabb3 {
    pub min: Point3,
    pub max: Point3,
    #[serde(default)]
    #[serde(with = "finite_f64")]
    pub absolute_tolerance: f64,
}

impl ExpectedAabb3 {
    #[must_use]
    pub const fn exact(bounds: Aabb3) -> Self {
        Self {
            min: bounds.min,
            max: bounds.max,
            absolute_tolerance: 0.0,
        }
    }

    fn mismatch(self, actual: Option<Aabb3>) -> Option<String> {
        let Some(actual) = actual else {
            return Some("expected finite bounds but the operation reported none".to_owned());
        };
        if !self.absolute_tolerance.is_finite() || self.absolute_tolerance < 0.0 {
            return Some(format!(
                "case has invalid absolute_tolerance {}",
                self.absolute_tolerance
            ));
        }

        let expected_values = [
            self.min.x, self.min.y, self.min.z, self.max.x, self.max.y, self.max.z,
        ];
        let actual_values = [
            actual.min.x,
            actual.min.y,
            actual.min.z,
            actual.max.x,
            actual.max.y,
            actual.max.z,
        ];
        if expected_values
            .into_iter()
            .zip(actual_values)
            .all(|(expected, actual)| {
                expected.is_finite()
                    && actual.is_finite()
                    && (expected - actual).abs() <= self.absolute_tolerance
            })
        {
            None
        } else {
            Some(format!(
                "bounds mismatch: expected [{} .. {}] ± {}, got {actual}",
                self.min, self.max, self.absolute_tolerance
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedMeasures {
    #[serde(with = "finite_f64")]
    pub surface_area: f64,
    #[serde(with = "finite_f64")]
    pub volume: f64,
    pub centroid: Point3,
    #[serde(default)]
    #[serde(with = "finite_f64")]
    pub absolute_tolerance: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedHistory {
    pub record_count: usize,
    pub relation: HistoryRelation,
    pub all_records_one_to_one: bool,
    pub complete_entity_mapping: bool,
}

impl ExpectedHistory {
    fn mismatch(self, actual: &OperationReport) -> Option<String> {
        let actual_one_to_one = actual
            .history
            .iter()
            .all(|record| record.inputs.len() == 1 && record.outputs.len() == 1);
        let complete_entity_mapping = complete_one_to_one_entity_mapping(actual);
        if actual.history.len() == self.record_count
            && actual
                .history
                .iter()
                .all(|record| record.relation == self.relation)
            && actual_one_to_one == self.all_records_one_to_one
            && complete_entity_mapping == self.complete_entity_mapping
        {
            None
        } else {
            Some(format!(
                "history mismatch: expected {} {:?} records with all_records_one_to_one={} and complete_entity_mapping={}, got {} records with relations {:?}, all_records_one_to_one={actual_one_to_one}, and complete_entity_mapping={complete_entity_mapping}",
                self.record_count,
                self.relation,
                self.all_records_one_to_one,
                self.complete_entity_mapping,
                actual.history.len(),
                actual
                    .history
                    .iter()
                    .map(|record| record.relation)
                    .collect::<Vec<_>>()
            ))
        }
    }
}

fn complete_one_to_one_entity_mapping(report: &OperationReport) -> bool {
    if report.history.len() as u64 != report.topology.total() {
        return false;
    }

    let mut inputs = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for record in &report.history {
        let ([input], [output]) = (record.inputs.as_slice(), record.outputs.as_slice()) else {
            return false;
        };
        if input.snapshot != report.input_snapshot
            || output.snapshot != report.output_snapshot
            || input.kind != output.kind
            || input.entity != output.entity
            || !inputs.insert(*input)
            || !outputs.insert(*output)
        {
            return false;
        }
    }

    [
        artificer_protocol::EntityKind::Vertex,
        artificer_protocol::EntityKind::Edge,
        artificer_protocol::EntityKind::Coedge,
        artificer_protocol::EntityKind::Loop,
        artificer_protocol::EntityKind::Face,
        artificer_protocol::EntityKind::Shell,
        artificer_protocol::EntityKind::Solid,
    ]
    .into_iter()
    .all(|kind| {
        let input_count = inputs.iter().filter(|entity| entity.kind == kind).count() as u64;
        let output_count = outputs.iter().filter(|entity| entity.kind == kind).count() as u64;
        input_count == report.topology.get(kind) && output_count == report.topology.get(kind)
    })
}

impl ExpectedMeasures {
    fn mismatch(self, actual: SnapshotMeasures) -> Option<String> {
        if !self.absolute_tolerance.is_finite() || self.absolute_tolerance < 0.0 {
            return Some(format!(
                "case has invalid measure absolute_tolerance {}",
                self.absolute_tolerance
            ));
        }
        let Some(centroid) = actual.centroid else {
            return Some("expected a finite centroid but the snapshot reported none".to_owned());
        };
        let expected = [
            self.surface_area,
            self.volume,
            self.centroid.x,
            self.centroid.y,
            self.centroid.z,
        ];
        let observed = [
            actual.surface_area,
            actual.volume,
            centroid.x,
            centroid.y,
            centroid.z,
        ];
        if expected
            .into_iter()
            .zip(observed)
            .all(|(expected, observed)| {
                expected.is_finite()
                    && observed.is_finite()
                    && (expected - observed).abs() <= self.absolute_tolerance
            })
        {
            None
        } else {
            Some(format!(
                "measure mismatch: expected area {}, volume {}, centroid {} ± {}; got area {}, volume {}, centroid {}",
                self.surface_area,
                self.volume,
                self.centroid,
                self.absolute_tolerance,
                actual.surface_area,
                actual.volume,
                centroid
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandJournal {
    pub schema_version: u32,
    pub protocol_version: ProtocolVersion,
    pub case_id: String,
    pub precision: PrecisionPolicy,
    pub initial_snapshot: SnapshotId,
    pub entries: Vec<JournalEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub step_id: String,
    pub input_snapshot: SnapshotId,
    pub request: ExecuteRequest,
    pub observed: JournalOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum JournalOutcome {
    Success {
        report: OperationReport,
    },
    Error {
        error: KernelError,
        retained_snapshot: SnapshotId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseFailure {
    pub step_id: String,
    pub message: String,
}

pub struct CaseRun {
    pub case_id: String,
    pub failures: Vec<CaseFailure>,
    pub journal: CommandJournal,
    pub final_report: Option<OperationReport>,
    pub final_scene: DebugScene,
}

/// Portable, bounded evidence emitted whenever a declarative case disagrees
/// with its contract. The bundle contains no process-local paths or timestamps,
/// so identical failures produce byte-identical JSON on supported platforms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureBundleManifest {
    pub schema_version: u32,
    pub case_id: String,
    pub protocol_version: ProtocolVersion,
    pub journal_schema_version: u32,
    pub final_digest: Option<SemanticDigest>,
    pub failures: Vec<CaseFailure>,
}

pub struct FailureBundle {
    pub manifest: FailureBundleManifest,
    pub journal_json: String,
    pub scene_svg: String,
}

impl CaseRun {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    #[must_use]
    pub fn final_digest(&self) -> Option<SemanticDigest> {
        self.final_report
            .as_ref()
            .map(|report| report.semantic_digest)
    }
}

pub struct ReplayRun {
    pub case_id: String,
    pub failures: Vec<CaseFailure>,
    pub final_report: Option<OperationReport>,
    pub final_scene: DebugScene,
}

impl ReplayRun {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum TestkitError {
    #[error("case schema version {actual} is unsupported; expected {expected}")]
    UnsupportedCaseSchema { actual: u32, expected: u32 },
    #[error("journal schema version {actual} is unsupported; expected {expected}")]
    UnsupportedJournalSchema { actual: u32, expected: u32 },
    #[error("protocol version {actual} is unsupported; expected {expected}")]
    UnsupportedProtocol {
        actual: ProtocolVersion,
        expected: ProtocolVersion,
    },
    #[error("case `{case_id}` has no steps")]
    EmptyCase { case_id: String },
    #[error("case `{case_id}` contains duplicate step id `{step_id}`")]
    DuplicateStepId { case_id: String, step_id: String },
    #[error("case `{case_id}` requires unsupported capability `{capability}`")]
    UnsupportedCapability { case_id: String, capability: String },
    #[error("step `{step_id}` in case `{case_id}` uses undeclared capability `{capability}`")]
    MissingCapabilityDeclaration {
        case_id: String,
        step_id: String,
        capability: String,
    },
    #[error(
        "step `{step_id}` in case `{case_id}` has an invalid target entity reference: {message}"
    )]
    InvalidTargetEntityReference {
        case_id: String,
        step_id: String,
        message: String,
    },
    #[error("step `{step_id}` in case `{case_id}` could not resolve its target entity: {message}")]
    TargetEntityResolution {
        case_id: String,
        step_id: String,
        message: String,
    },
    #[error("JSON encoding or decoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no successful operation produced display geometry")]
    SceneUnavailable,
}

pub fn parse_case_json(json: &str) -> Result<KernelCase, TestkitError> {
    let case: KernelCase = serde_json::from_str(json)?;
    validate_case(&case)?;
    Ok(case)
}

pub fn parse_journal_json(json: &str) -> Result<CommandJournal, TestkitError> {
    let journal: CommandJournal = serde_json::from_str(json)?;
    validate_journal(&journal)?;
    Ok(journal)
}

pub fn to_pretty_json<T: Serialize>(value: &T) -> Result<String, TestkitError> {
    let mut encoded = serde_json::to_string_pretty(value)?;
    encoded.push('\n');
    Ok(encoded)
}

pub fn failure_bundle(run: &CaseRun) -> Result<FailureBundle, TestkitError> {
    let scene_svg = scene_svg(run).unwrap_or_else(|_| failure_only_svg(run));
    Ok(FailureBundle {
        manifest: FailureBundleManifest {
            schema_version: FAILURE_BUNDLE_SCHEMA_VERSION,
            case_id: run.case_id.clone(),
            protocol_version: run.journal.protocol_version,
            journal_schema_version: run.journal.schema_version,
            final_digest: run.final_digest(),
            failures: run.failures.clone(),
        },
        journal_json: to_pretty_json(&run.journal)?,
        scene_svg,
    })
}

fn failure_only_svg(run: &CaseRun) -> String {
    let mut svg = String::new();
    writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="640" viewBox="0 0 960 640">"#
    )
    .expect("String write");
    writeln!(svg, "  <title>{}</title>", escape_xml(&run.case_id)).expect("String write");
    svg.push_str("  <rect width=\"960\" height=\"640\" fill=\"#0d1118\"/>\n");
    svg.push_str("  <text x=\"32\" y=\"64\" fill=\"#ff7b7b\" font-family=\"monospace\" font-size=\"24\">No valid body was published</text>\n");
    for (index, failure) in run.failures.iter().take(12).enumerate() {
        writeln!(svg, "  <text x=\"32\" y=\"{}\" fill=\"#e5eaf2\" font-family=\"monospace\" font-size=\"14\">{}: {}</text>", 104 + index * 30, escape_xml(&failure.step_id), escape_xml(&failure.message)).expect("String write");
    }
    svg.push_str("</svg>\n");
    svg
}

pub fn validate_case(case: &KernelCase) -> Result<(), TestkitError> {
    if case.schema_version != KERNEL_CASE_SCHEMA_VERSION {
        return Err(TestkitError::UnsupportedCaseSchema {
            actual: case.schema_version,
            expected: KERNEL_CASE_SCHEMA_VERSION,
        });
    }
    if case.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(TestkitError::UnsupportedProtocol {
            actual: case.protocol_version,
            expected: CURRENT_PROTOCOL_VERSION,
        });
    }
    if case.steps.is_empty() {
        return Err(TestkitError::EmptyCase {
            case_id: case.case_id.clone(),
        });
    }
    for capability in &case.required_capabilities {
        if !matches!(
            capability.as_str(),
            "native.make_cuboid.v0"
                | "native.make_revolved_annulus.v0"
                | "native.transform_snapshot.v0"
                | "native.extrude_polygon.v0"
                | "native.extrude_planar_profile.v0"
                | "native.revolve_planar_profile.v0"
                | "native.extrude_face_profile.v0"
                | "native.extrude_face_planar_profile.v0"
                | "native.push_pull_face.v0"
                | "native.drill_hole.v0"
                | "native.add_rib.v0"
                | "native.mirror_snapshot.v0"
                | "native.linear_pattern_snapshot.v0"
                | "native.finish_edge.v0"
        ) {
            return Err(TestkitError::UnsupportedCapability {
                case_id: case.case_id.clone(),
                capability: capability.clone(),
            });
        }
    }

    let mut step_ids = std::collections::BTreeSet::new();
    for step in &case.steps {
        if step_ids.contains(step.step_id.as_str()) {
            return Err(TestkitError::DuplicateStepId {
                case_id: case.case_id.clone(),
                step_id: step.step_id.clone(),
            });
        }
        if let Some(reference) = &step.target_entity {
            validate_target_entity_reference(case, step, reference, &step_ids)?;
        }
        step_ids.insert(step.step_id.as_str());
        let required = match &step.command {
            KernelCommand::MakeCuboid { .. } => "native.make_cuboid.v0",
            KernelCommand::MakeRevolvedAnnulus { .. } => "native.make_revolved_annulus.v0",
            KernelCommand::TransformSnapshot { .. } => "native.transform_snapshot.v0",
            KernelCommand::ExtrudePolygon { .. } => "native.extrude_polygon.v0",
            KernelCommand::ExtrudePlanarProfile { .. } => "native.extrude_planar_profile.v0",
            KernelCommand::RevolvePlanarProfile { .. } => "native.revolve_planar_profile.v0",
            KernelCommand::ExtrudeFaceProfile { .. } => "native.extrude_face_profile.v0",
            KernelCommand::ExtrudeFacePlanarProfile { .. } => {
                "native.extrude_face_planar_profile.v0"
            }
            KernelCommand::PushPullFace { .. } => "native.push_pull_face.v0",
            KernelCommand::DrillHole { .. } => "native.drill_hole.v0",
            KernelCommand::AddRib { .. } => "native.add_rib.v0",
            KernelCommand::MirrorSnapshot { .. } => "native.mirror_snapshot.v0",
            KernelCommand::LinearPatternSnapshot { .. } => "native.linear_pattern_snapshot.v0",
            KernelCommand::FinishEdge { .. } => "native.finish_edge.v0",
            KernelCommand::FinishEdges { .. } => "native.finish_edges.v0",
        };
        if !case
            .required_capabilities
            .iter()
            .any(|capability| capability == required)
        {
            return Err(TestkitError::MissingCapabilityDeclaration {
                case_id: case.case_id.clone(),
                step_id: step.step_id.clone(),
                capability: required.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_target_entity_reference(
    case: &KernelCase,
    step: &CaseStep,
    reference: &HistoryOutputReference,
    prior_step_ids: &BTreeSet<&str>,
) -> Result<(), TestkitError> {
    let invalid = |message: String| TestkitError::InvalidTargetEntityReference {
        case_id: case.case_id.clone(),
        step_id: step.step_id.clone(),
        message,
    };

    if reference.from_step.is_empty() {
        return Err(invalid("`from_step` must not be empty".to_owned()));
    }
    if reference.role.is_empty() {
        return Err(invalid("`role` must not be empty".to_owned()));
    }
    if !prior_step_ids.contains(reference.from_step.as_str()) {
        return Err(invalid(format!(
            "`from_step` must name an earlier step, got `{}`",
            reference.from_step
        )));
    }
    match &step.command {
        KernelCommand::ExtrudeFaceProfile { .. }
        | KernelCommand::ExtrudeFacePlanarProfile { .. }
        | KernelCommand::PushPullFace { .. }
            if reference.kind == EntityKind::Face =>
        {
            Ok(())
        }
        KernelCommand::ExtrudeFaceProfile { .. }
        | KernelCommand::ExtrudeFacePlanarProfile { .. }
        | KernelCommand::PushPullFace { .. } => Err(invalid(format!(
            "a face-operation target must have kind `face`, got `{}`",
            reference.kind
        ))),
        _ => Err(invalid(
            "only supported face-operation targets allow case-layer late binding".to_owned(),
        )),
    }
}

pub fn validate_journal(journal: &CommandJournal) -> Result<(), TestkitError> {
    if journal.schema_version != COMMAND_JOURNAL_SCHEMA_VERSION {
        return Err(TestkitError::UnsupportedJournalSchema {
            actual: journal.schema_version,
            expected: COMMAND_JOURNAL_SCHEMA_VERSION,
        });
    }
    if journal.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(TestkitError::UnsupportedProtocol {
            actual: journal.protocol_version,
            expected: CURRENT_PROTOCOL_VERSION,
        });
    }
    if let Some(entry) = journal
        .entries
        .iter()
        .find(|entry| entry.request.protocol_version != journal.protocol_version)
    {
        return Err(TestkitError::UnsupportedProtocol {
            actual: entry.request.protocol_version,
            expected: journal.protocol_version,
        });
    }
    Ok(())
}

fn resolve_step_command(
    case: &KernelCase,
    step: &CaseStep,
    prior_entries: &[JournalEntry],
    current_snapshot: SnapshotId,
) -> Result<KernelCommand, TestkitError> {
    let Some(reference) = &step.target_entity else {
        return Ok(step.command.clone());
    };
    let target = resolve_history_output(case, step, reference, prior_entries, current_snapshot)?;
    let mut command = step.command.clone();
    match &mut command {
        KernelCommand::ExtrudeFaceProfile { target_face, .. }
        | KernelCommand::ExtrudeFacePlanarProfile { target_face, .. }
        | KernelCommand::PushPullFace { target_face, .. } => *target_face = target,
        _ => unreachable!("validated target references only apply to face commands"),
    }
    Ok(command)
}

fn resolve_history_output(
    case: &KernelCase,
    step: &CaseStep,
    reference: &HistoryOutputReference,
    prior_entries: &[JournalEntry],
    current_snapshot: SnapshotId,
) -> Result<EntityRef, TestkitError> {
    let resolution_error = |message: String| TestkitError::TargetEntityResolution {
        case_id: case.case_id.clone(),
        step_id: step.step_id.clone(),
        message,
    };
    let Some((source_index, source_entry)) = prior_entries
        .iter()
        .enumerate()
        .find(|(_, entry)| entry.step_id == reference.from_step)
    else {
        return Err(resolution_error(format!(
            "source step `{}` has no recorded outcome",
            reference.from_step
        )));
    };
    let JournalOutcome::Success {
        report: source_report,
    } = &source_entry.observed
    else {
        return Err(resolution_error(format!(
            "source step `{}` did not publish a snapshot",
            reference.from_step
        )));
    };

    let source_outputs = source_report
        .history
        .iter()
        .filter(|record| {
            record.role.as_ref().is_some_and(|role| {
                role.name == reference.role
                    && reference
                        .ordinal
                        .is_none_or(|ordinal| role.ordinal == Some(ordinal))
            })
        })
        .flat_map(|record| record.outputs.iter().copied())
        .filter(|output| output.kind == reference.kind)
        .collect::<BTreeSet<_>>();
    let mut target = unique_history_output(
        source_outputs,
        reference,
        &reference.from_step,
        &resolution_error,
    )?;

    for entry in &prior_entries[source_index + 1..] {
        let JournalOutcome::Success { report } = &entry.observed else {
            continue;
        };
        if target.snapshot != report.input_snapshot {
            return Err(resolution_error(format!(
                "entity selected from `{}` belongs to snapshot {}, but step `{}` consumes {}",
                reference.from_step, target.snapshot, entry.step_id, report.input_snapshot
            )));
        }
        let descendants = report
            .history
            .iter()
            .filter(|record| record.inputs.contains(&target))
            .flat_map(|record| record.outputs.iter().copied())
            .filter(|output| output.kind == reference.kind)
            .collect::<BTreeSet<_>>();
        target = unique_history_output(descendants, reference, &entry.step_id, &resolution_error)?;
    }

    if target.snapshot != current_snapshot {
        return Err(resolution_error(format!(
            "resolved entity belongs to snapshot {}, but the current snapshot is {}",
            target.snapshot, current_snapshot
        )));
    }
    Ok(target)
}

fn unique_history_output(
    outputs: BTreeSet<EntityRef>,
    reference: &HistoryOutputReference,
    mapping_step: &str,
    error: &impl Fn(String) -> TestkitError,
) -> Result<EntityRef, TestkitError> {
    if outputs.len() != 1 {
        let ordinal = reference
            .ordinal
            .map_or_else(String::new, |ordinal| format!("[{ordinal}]"));
        return Err(error(format!(
            "history at step `{mapping_step}` resolved {} `{}`{ordinal} output(s) of kind `{}`; expected exactly one",
            outputs.len(),
            reference.role,
            reference.kind
        )));
    }
    Ok(*outputs
        .first()
        .expect("the cardinality check guarantees one history output"))
}

pub fn run_case(case: &KernelCase) -> Result<CaseRun, TestkitError> {
    validate_case(case)?;

    let mut snapshot = NativeKernel::empty();
    let initial_snapshot = snapshot.id();
    let mut entries = Vec::with_capacity(case.steps.len());
    let mut failures = Vec::new();
    let mut final_report = None;

    for step in &case.steps {
        let input_snapshot = snapshot.id();
        let command = resolve_step_command(case, step, &entries, input_snapshot)?;
        let request = ExecuteRequest {
            protocol_version: case.protocol_version,
            request_id: RequestId::new(format!("{}::{}", case.case_id, step.step_id)),
            expected_snapshot: input_snapshot,
            precision: case.precision,
            command,
        };

        match NativeKernel::execute(&snapshot, &request, &CancellationToken::default()) {
            Ok(outcome) => {
                compare_success(
                    &step.step_id,
                    &step.expected,
                    &outcome.report,
                    outcome.snapshot.measures(),
                    &mut failures,
                );
                if outcome.snapshot.id() != outcome.report.output_snapshot {
                    failures.push(CaseFailure {
                        step_id: step.step_id.clone(),
                        message: format!(
                            "outcome snapshot {} disagrees with report snapshot {}",
                            outcome.snapshot.id(),
                            outcome.report.output_snapshot
                        ),
                    });
                }
                entries.push(JournalEntry {
                    step_id: step.step_id.clone(),
                    input_snapshot,
                    request,
                    observed: JournalOutcome::Success {
                        report: outcome.report.clone(),
                    },
                });
                final_report = Some(outcome.report);
                snapshot = outcome.snapshot;
            }
            Err(error) => {
                compare_error(&step.step_id, &step.expected, &error, &mut failures);
                entries.push(JournalEntry {
                    step_id: step.step_id.clone(),
                    input_snapshot,
                    request,
                    observed: JournalOutcome::Error {
                        error,
                        retained_snapshot: snapshot.id(),
                    },
                });
            }
        }
    }

    let final_scene = NativeKernel::debug_scene(&snapshot);
    Ok(CaseRun {
        case_id: case.case_id.clone(),
        failures,
        journal: CommandJournal {
            schema_version: COMMAND_JOURNAL_SCHEMA_VERSION,
            protocol_version: case.protocol_version,
            case_id: case.case_id.clone(),
            precision: case.precision,
            initial_snapshot,
            entries,
        },
        final_report,
        final_scene,
    })
}

pub fn replay_journal(journal: &CommandJournal) -> Result<ReplayRun, TestkitError> {
    validate_journal(journal)?;

    let mut snapshot = NativeKernel::empty();
    let mut failures = Vec::new();
    let mut final_report = None;

    if snapshot.id() != journal.initial_snapshot {
        failures.push(CaseFailure {
            step_id: "initial_snapshot".to_owned(),
            message: format!(
                "empty snapshot changed: journal {}, replay {}",
                journal.initial_snapshot,
                snapshot.id()
            ),
        });
    }

    for entry in &journal.entries {
        if snapshot.id() != entry.input_snapshot {
            failures.push(CaseFailure {
                step_id: entry.step_id.clone(),
                message: format!(
                    "input snapshot mismatch: journal {}, replay {}",
                    entry.input_snapshot,
                    snapshot.id()
                ),
            });
        }

        let replayed =
            NativeKernel::execute(&snapshot, &entry.request, &CancellationToken::default());
        match (&entry.observed, replayed) {
            (JournalOutcome::Success { report: recorded }, Ok(outcome)) => {
                if &outcome.report != recorded {
                    failures.push(CaseFailure {
                        step_id: entry.step_id.clone(),
                        message: report_mismatch(recorded, &outcome.report),
                    });
                }
                final_report = Some(outcome.report);
                snapshot = outcome.snapshot;
            }
            (
                JournalOutcome::Error {
                    error: recorded,
                    retained_snapshot,
                },
                Err(actual),
            ) => {
                if &actual != recorded {
                    failures.push(CaseFailure {
                        step_id: entry.step_id.clone(),
                        message: format!(
                            "error replay mismatch: recorded {recorded:?}, replayed {actual:?}"
                        ),
                    });
                }
                if snapshot.id() != *retained_snapshot {
                    failures.push(CaseFailure {
                        step_id: entry.step_id.clone(),
                        message: format!(
                            "failed transaction changed snapshot: recorded {}, replay {}",
                            retained_snapshot,
                            snapshot.id()
                        ),
                    });
                }
            }
            (JournalOutcome::Success { .. }, Err(actual)) => failures.push(CaseFailure {
                step_id: entry.step_id.clone(),
                message: format!("recorded success replayed as error: {actual}"),
            }),
            (JournalOutcome::Error { error, .. }, Ok(outcome)) => {
                failures.push(CaseFailure {
                    step_id: entry.step_id.clone(),
                    message: format!(
                        "recorded error {} replayed as success {}",
                        error.code, outcome.report.semantic_digest
                    ),
                });
                final_report = Some(outcome.report);
                snapshot = outcome.snapshot;
            }
        }
    }

    let final_scene = NativeKernel::debug_scene(&snapshot);
    Ok(ReplayRun {
        case_id: journal.case_id.clone(),
        failures,
        final_report,
        final_scene,
    })
}

fn compare_success(
    step_id: &str,
    expected: &ExpectedOutcome,
    actual: &OperationReport,
    actual_measures: SnapshotMeasures,
    failures: &mut Vec<CaseFailure>,
) {
    let ExpectedOutcome::Success {
        topology,
        bounds,
        validation_valid,
        semantic_digest,
        measures,
        history,
    } = expected
    else {
        let ExpectedOutcome::Error { code, .. } = expected else {
            unreachable!();
        };
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!(
                "expected error {code}, operation succeeded with digest {}",
                actual.semantic_digest
            ),
        });
        return;
    };

    if actual.topology != *topology {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!(
                "topology mismatch: expected {topology}, got {}",
                actual.topology
            ),
        });
    }
    if let Some(message) = bounds.mismatch(actual.bounds) {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message,
        });
    }
    if actual.validation.valid != *validation_valid {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!(
                "validation status mismatch: expected {validation_valid}, got {}",
                actual.validation.valid
            ),
        });
    }
    if semantic_digest.is_some_and(|expected| expected != actual.semantic_digest) {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!(
                "semantic digest mismatch: expected {}, got {}",
                semantic_digest.expect("checked above"),
                actual.semantic_digest
            ),
        });
    }
    if let Some(message) = measures.and_then(|expected| expected.mismatch(actual_measures)) {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message,
        });
    }
    if let Some(message) = history.and_then(|expected| expected.mismatch(actual)) {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message,
        });
    }
}

fn compare_error(
    step_id: &str,
    expected: &ExpectedOutcome,
    actual: &KernelError,
    failures: &mut Vec<CaseFailure>,
) {
    let ExpectedOutcome::Error {
        code,
        diagnostic_codes,
    } = expected
    else {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!("expected success, operation returned {}", actual.code),
        });
        return;
    };

    if actual.code != *code {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!("error mismatch: expected {code}, got {}", actual.code),
        });
    }
    let mut expected_codes = diagnostic_codes.clone();
    expected_codes.sort();
    let mut actual_codes = actual
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.clone())
        .collect::<Vec<_>>();
    actual_codes.sort();
    if actual_codes != expected_codes {
        failures.push(CaseFailure {
            step_id: step_id.to_owned(),
            message: format!(
                "diagnostic codes mismatch: expected {expected_codes:?}, got {actual_codes:?}"
            ),
        });
    }
}

fn report_mismatch(recorded: &OperationReport, actual: &OperationReport) -> String {
    let differing_field = if recorded.input_snapshot != actual.input_snapshot {
        "input_snapshot".to_owned()
    } else if recorded.output_snapshot != actual.output_snapshot {
        "output_snapshot".to_owned()
    } else if recorded.semantic_digest != actual.semantic_digest {
        "semantic_digest".to_owned()
    } else if recorded.topology != actual.topology {
        "topology".to_owned()
    } else if recorded.bounds != actual.bounds {
        format!(
            "bounds (recorded {:?}, replayed {:?})",
            recorded.bounds, actual.bounds
        )
    } else if recorded.history != actual.history {
        "history".to_owned()
    } else if recorded.validation != actual.validation {
        "validation".to_owned()
    } else {
        "warnings".to_owned()
    };
    format!(
        "operation report replay mismatch in {differing_field}: recorded digest {} / {}, replayed {} / {}",
        recorded.semantic_digest, recorded.topology, actual.semantic_digest, actual.topology
    )
}

pub fn scene_svg(run: &CaseRun) -> Result<String, TestkitError> {
    let report = run
        .final_report
        .as_ref()
        .ok_or(TestkitError::SceneUnavailable)?;
    render_scene_svg(&run.case_id, &run.final_scene, report)
}

pub fn render_scene_svg(
    title: &str,
    scene: &DebugScene,
    report: &OperationReport,
) -> Result<String, TestkitError> {
    let points = scene
        .triangles
        .iter()
        .flat_map(|triangle| triangle.vertices)
        .chain(scene.edges.iter().flat_map(|edge| edge.endpoints))
        .collect::<Vec<_>>();
    if points.is_empty() {
        return Err(TestkitError::SceneUnavailable);
    }

    let projection = SvgProjection::fit(&points).ok_or(TestkitError::SceneUnavailable)?;
    let mut triangles = scene.triangles.iter().collect::<Vec<_>>();
    triangles.sort_by(|left, right| {
        triangle_depth(left.vertices)
            .total_cmp(&triangle_depth(right.vertices))
            .then_with(|| left.role.cmp(&right.role))
            .then_with(|| left.source_face.cmp(&right.source_face))
    });
    let mut edges = scene.edges.iter().collect::<Vec<_>>();
    edges.sort_by_key(|edge| edge.source_edge);

    let mut svg = String::new();
    writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="640" viewBox="0 0 960 640">"#
    )
    .expect("writing to String cannot fail");
    writeln!(svg, "  <title>{}</title>", escape_xml(title)).expect("writing to String cannot fail");
    writeln!(
        svg,
        "  <desc>snapshot {} · digest {} · {}</desc>",
        report.output_snapshot, report.semantic_digest, report.topology
    )
    .expect("writing to String cannot fail");
    svg.push_str("  <rect width=\"960\" height=\"640\" fill=\"#0d1118\"/>\n");
    svg.push_str("  <g stroke-linejoin=\"round\">\n");
    for triangle in triangles {
        let projected = triangle.vertices.map(|point| projection.point(point));
        writeln!(
            svg,
            "    <polygon points=\"{},{} {},{} {},{}\" fill=\"{}\" fill-opacity=\"0.86\" data-face=\"{}\" data-role=\"{}\"/>",
            svg_number(projected[0][0]),
            svg_number(projected[0][1]),
            svg_number(projected[1][0]),
            svg_number(projected[1][1]),
            svg_number(projected[2][0]),
            svg_number(projected[2][1]),
            face_color(triangle.role),
            triangle.source_face.entity,
            face_role_name(triangle.role),
        )
        .expect("writing to String cannot fail");
    }
    svg.push_str("  </g>\n  <g fill=\"none\" stroke=\"#d5e2ee\" stroke-width=\"2\" stroke-linecap=\"round\">\n");
    for edge in edges {
        let [start, end] = edge.endpoints.map(|point| projection.point(point));
        writeln!(
            svg,
            "    <line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" data-edge=\"{}\"/>",
            svg_number(start[0]),
            svg_number(start[1]),
            svg_number(end[0]),
            svg_number(end[1]),
            edge.source_edge.entity,
        )
        .expect("writing to String cannot fail");
    }
    svg.push_str("  </g>\n");
    writeln!(
        svg,
        "  <text x=\"28\" y=\"606\" fill=\"#e5eaf2\" font-family=\"monospace\" font-size=\"15\">{}</text>",
        escape_xml(&format!("{} · {}", report.topology, report.semantic_digest))
    )
    .expect("writing to String cannot fail");
    svg.push_str("</svg>\n");
    Ok(svg)
}

#[derive(Clone, Copy)]
struct SvgProjection {
    center: [f64; 2],
    scale: f64,
}

impl SvgProjection {
    fn fit(points: &[Point3]) -> Option<Self> {
        let mut min = [f64::INFINITY; 2];
        let mut max = [f64::NEG_INFINITY; 2];
        for point in points.iter().copied().filter(|point| point.is_finite()) {
            let projected = raw_projection(point);
            for axis in 0..2 {
                min[axis] = min[axis].min(projected[axis]);
                max[axis] = max[axis].max(projected[axis]);
            }
        }
        let width = max[0] - min[0];
        let height = max[1] - min[1];
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return None;
        }
        Some(Self {
            center: [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5],
            scale: (820.0 / width).min(500.0 / height),
        })
    }

    fn point(self, point: Point3) -> [f64; 2] {
        let projected = raw_projection(point);
        [
            480.0 + (projected[0] - self.center[0]) * self.scale,
            300.0 + (projected[1] - self.center[1]) * self.scale,
        ]
    }
}

fn raw_projection(point: Point3) -> [f64; 2] {
    const COS_30: f64 = 0.866_025_403_784_438_6;
    [
        COS_30 * (point.x - point.y),
        0.5 * (point.x + point.y) - point.z,
    ]
}

fn triangle_depth(vertices: [Point3; 3]) -> f64 {
    vertices
        .iter()
        .map(|point| point.x + point.y + point.z)
        .sum::<f64>()
        / 3.0
}

const fn face_color(role: FaceRole) -> &'static str {
    match role {
        FaceRole::NegativeX => "#287e80",
        FaceRole::PositiveX => "#2f989a",
        FaceRole::NegativeY => "#5159a8",
        FaceRole::PositiveY => "#6a74d7",
        FaceRole::NegativeZ => "#386f94",
        FaceRole::PositiveZ => "#52a5de",
        FaceRole::ExtrusionBottom => "#386f94",
        FaceRole::ExtrusionTop => "#52a5de",
        FaceRole::ExtrusionSide(_) => "#2f989a",
        FaceRole::FeatureEnd => "#5bdb9f",
        FaceRole::FeatureSide(_) => "#3fa77b",
    }
}

fn face_role_name(role: FaceRole) -> String {
    match role {
        FaceRole::NegativeX => "negative-x".to_owned(),
        FaceRole::PositiveX => "positive-x".to_owned(),
        FaceRole::NegativeY => "negative-y".to_owned(),
        FaceRole::PositiveY => "positive-y".to_owned(),
        FaceRole::NegativeZ => "negative-z".to_owned(),
        FaceRole::PositiveZ => "positive-z".to_owned(),
        FaceRole::ExtrusionBottom => "extrusion-bottom".to_owned(),
        FaceRole::ExtrusionTop => "extrusion-top".to_owned(),
        FaceRole::ExtrusionSide(ordinal) => format!("extrusion-side-{ordinal}"),
        FaceRole::FeatureEnd => "feature-end".to_owned(),
        FaceRole::FeatureSide(ordinal) => format!("feature-side-{ordinal}"),
    }
}

fn svg_number(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    format!("{value:.3}")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL_CASE: &str = include_str!("../../../tests/cases/m0-cuboid.json");
    const CANONICAL_SVG: &str = include_str!("../../../tests/goldens/m0-cuboid.svg");
    const ZERO_WIDTH_CASE: &str = include_str!("../../../tests/cases/m0-cuboid-zero-width.json");
    const TRANSFORM_CASE: &str = include_str!("../../../tests/cases/m1-transform-similarity.json");
    const TRANSFORM_REJECTION_CASE: &str =
        include_str!("../../../tests/cases/m1-transform-reject-and-continue.json");
    const EXTRUDE_RECTANGLE_CASE: &str =
        include_str!("../../../tests/cases/m4-extrude-rectangle.json");
    const EXTRUDE_CONCAVE_CASE: &str = include_str!("../../../tests/cases/m4-extrude-concave.json");
    const FACE_ADD_CASE: &str = include_str!("../../../tests/cases/m4-face-add.json");
    const FACE_CUT_CASE: &str = include_str!("../../../tests/cases/m4-face-cut.json");
    const FACE_CHAIN_CASE: &str = include_str!("../../../tests/cases/m4-face-chain.json");
    const FACE_PUSH_PULL_CASE: &str = include_str!("../../../tests/cases/m4-face-push-pull.json");

    #[test]
    fn canonical_case_round_trips_without_losing_protocol_data() {
        let case = parse_case_json(CANONICAL_CASE).unwrap();
        assert_eq!(case.case_id, "m0.cuboid-2x3x4");
        let encoded = to_pretty_json(&case).unwrap();
        assert_eq!(parse_case_json(&encoded).unwrap(), case);
    }

    #[test]
    fn canonical_case_runs_and_replays_exactly() {
        let case = parse_case_json(CANONICAL_CASE).unwrap();
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert_eq!(run.journal.entries.len(), 1);

        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        assert_eq!(decoded, run.journal);
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert_eq!(replay.final_report, run.final_report);
    }

    #[test]
    fn exact_planar_profile_capabilities_run_and_late_bind_face_targets() {
        let mut standalone = parse_case_json(EXTRUDE_RECTANGLE_CASE).unwrap();
        standalone.required_capabilities = vec!["native.extrude_planar_profile.v0".to_owned()];
        let KernelCommand::ExtrudePolygon {
            frame,
            vertices,
            distance,
        } = standalone.steps[0].command.clone()
        else {
            panic!("rectangle fixture must start with a polygon extrusion");
        };
        standalone.steps[0].command = KernelCommand::ExtrudePlanarProfile {
            frame,
            profile: artificer_protocol::PlanarProfile2::from_polygon(&vertices),
            distance,
        };
        let ExpectedOutcome::Success {
            semantic_digest,
            history,
            ..
        } = &mut standalone.steps[0].expected
        else {
            panic!("rectangle fixture must expect success");
        };
        *semantic_digest = None;
        *history = None;
        let standalone_run = run_case(&standalone).unwrap();
        assert!(standalone_run.passed(), "{:?}", standalone_run.failures);
        let standalone_replay = replay_journal(&standalone_run.journal).unwrap();
        assert!(
            standalone_replay.passed(),
            "{:?}",
            standalone_replay.failures
        );
        assert_eq!(standalone_replay.final_report, standalone_run.final_report);

        let mut targeted = parse_case_json(FACE_CHAIN_CASE).unwrap();
        targeted
            .required_capabilities
            .push("native.extrude_face_planar_profile.v0".to_owned());
        let KernelCommand::ExtrudeFaceProfile {
            target_face,
            frame,
            vertices,
            distance,
            operation,
        } = targeted.steps[1].command.clone()
        else {
            panic!("face-chain fixture must contain a selected-face extrusion");
        };
        targeted.steps[1].command = KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            frame,
            profile: artificer_protocol::PlanarProfile2::from_polygon(&vertices),
            distance,
            operation,
        };
        for step in &mut targeted.steps[1..] {
            let ExpectedOutcome::Success {
                semantic_digest,
                history,
                ..
            } = &mut step.expected
            else {
                panic!("face-chain feature steps must expect success");
            };
            *semantic_digest = None;
            *history = None;
        }
        let targeted_run = run_case(&targeted).unwrap();
        assert!(targeted_run.passed(), "{:?}", targeted_run.failures);
        let targeted_replay = replay_journal(&targeted_run.journal).unwrap();
        assert!(targeted_replay.passed(), "{:?}", targeted_replay.failures);
        assert_eq!(targeted_replay.final_report, targeted_run.final_report);
        let KernelCommand::ExtrudeFacePlanarProfile { target_face, .. } =
            &targeted_run.journal.entries[1].request.command
        else {
            panic!("late-bound journal must retain the exact planar face command");
        };
        assert_ne!(target_face.snapshot, SnapshotId::ZERO);
        assert_eq!(target_face.kind, EntityKind::Face);
    }

    #[test]
    fn similarity_case_runs_and_replays_exactly() {
        let case = parse_case_json(TRANSFORM_CASE).unwrap();
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert_eq!(run.journal.entries.len(), 2);
        // Transform reports contain nontrivial floating bounds; the disk JSON
        // path must preserve their exact f64 values for deterministic replay.
        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        assert_eq!(decoded, run.journal);
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert_eq!(replay.final_report, run.final_report);
        assert_eq!(run.final_scene.triangles.len(), 12);
        assert_eq!(run.final_scene.edges.len(), 12);
    }

    #[test]
    fn rejected_transform_retains_snapshot_and_later_step_continues() {
        let case = parse_case_json(TRANSFORM_REJECTION_CASE).unwrap();
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert_eq!(run.journal.entries.len(), 3);
        let first_output = match &run.journal.entries[0].observed {
            JournalOutcome::Success { report } => report.output_snapshot,
            JournalOutcome::Error { .. } => panic!("cuboid step must succeed"),
        };
        match &run.journal.entries[1].observed {
            JournalOutcome::Error {
                retained_snapshot, ..
            } => assert_eq!(*retained_snapshot, first_output),
            JournalOutcome::Success { .. } => panic!("zero-scale step must reject"),
        }
        assert_eq!(run.journal.entries[2].input_snapshot, first_output);
        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        assert_eq!(decoded, run.journal);
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert_eq!(replay.final_report, run.final_report);
    }

    #[test]
    fn convex_extrusion_case_runs_round_trips_and_replays_exactly() {
        let case = parse_case_json(EXTRUDE_RECTANGLE_CASE).unwrap();
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert_eq!(run.journal.entries.len(), 1);
        assert_eq!(run.final_scene.triangles.len(), 12);
        assert_eq!(run.final_scene.edges.len(), 12);
        assert!(
            run.final_scene
                .triangles
                .iter()
                .any(|triangle| triangle.role == FaceRole::ExtrusionBottom)
        );
        assert!(
            run.final_scene
                .triangles
                .iter()
                .any(|triangle| triangle.role == FaceRole::ExtrusionTop)
        );
        assert!((0..4).all(|ordinal| {
            run.final_scene
                .triangles
                .iter()
                .any(|triangle| triangle.role == FaceRole::ExtrusionSide(ordinal))
        }));

        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        assert_eq!(decoded, run.journal);
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert_eq!(replay.final_report, run.final_report);
    }

    #[test]
    fn concave_extrusion_case_runs_and_replays_with_exact_geometry() {
        let case = parse_case_json(EXTRUDE_CONCAVE_CASE).unwrap();
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert!(run.final_report.is_some());
        assert_eq!(run.final_scene.triangles.len(), 16);
        assert_eq!(run.final_scene.edges.len(), 15);

        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert!(replay.final_report.is_some());
    }

    #[test]
    fn selected_face_add_and_cut_cases_run_and_replay_exactly() {
        for source in [FACE_ADD_CASE, FACE_CUT_CASE] {
            let case = parse_case_json(source).unwrap();
            let run = run_case(&case).unwrap();
            assert!(run.passed(), "{}: {:?}", case.case_id, run.failures);
            assert_eq!(run.journal.entries.len(), 2);
            assert_eq!(run.final_scene.edges.len(), 24);
            assert_eq!(run.final_scene.triangles.len(), 28);
            assert!(
                run.final_scene
                    .triangles
                    .iter()
                    .any(|triangle| { triangle.role == FaceRole::FeatureEnd })
            );
            assert!((0..4).all(|ordinal| {
                run.final_scene
                    .triangles
                    .iter()
                    .any(|triangle| triangle.role == FaceRole::FeatureSide(ordinal))
            }));

            let journal_json = to_pretty_json(&run.journal).unwrap();
            let decoded = parse_journal_json(&journal_json).unwrap();
            assert_eq!(decoded, run.journal);
            let replay = replay_journal(&decoded).unwrap();
            assert!(replay.passed(), "{}: {:?}", case.case_id, replay.failures);
            assert_eq!(replay.final_report, run.final_report);
        }
    }

    #[test]
    fn selected_face_push_pull_case_runs_rebinds_and_replays_exactly() {
        let case = parse_case_json(FACE_PUSH_PULL_CASE).unwrap();
        assert_eq!(
            parse_case_json(&to_pretty_json(&case).unwrap()).unwrap(),
            case
        );
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert_eq!(run.journal.entries.len(), 3);
        assert_eq!(run.final_scene.edges.len(), 12);
        assert_eq!(run.final_scene.triangles.len(), 12);

        let pushed_target = unique_role_output(
            success_report(&run.journal.entries[1]),
            EntityKind::Face,
            "face_push_pull.target_face",
            None,
        );
        let KernelCommand::PushPullFace { target_face, .. } =
            &run.journal.entries[2].request.command
        else {
            panic!("third concrete journal command should remain push/pull")
        };
        assert_eq!(*target_face, pushed_target);

        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        assert_eq!(decoded, run.journal);
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert_eq!(replay.final_report, run.final_report);
    }

    #[test]
    fn chained_case_references_resolve_to_concrete_journal_commands_and_replay() {
        let case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        assert_eq!(
            parse_case_json(&to_pretty_json(&case).unwrap()).unwrap(),
            case
        );
        let literal_case = parse_case_json(FACE_ADD_CASE).unwrap();
        assert!(
            !to_pretty_json(&literal_case)
                .unwrap()
                .contains("target_entity")
        );

        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert_eq!(run.journal.entries.len(), 4);

        let base_face = unique_role_output(
            success_report(&run.journal.entries[0]),
            EntityKind::Face,
            "face",
            Some(1),
        );
        let boss_end = unique_role_output(
            success_report(&run.journal.entries[1]),
            EntityKind::Face,
            "face_extrude.boss.end_face",
            None,
        );
        let pocket_floor = unique_role_output(
            success_report(&run.journal.entries[2]),
            EntityKind::Face,
            "face_extrude.pocket.floor_face",
            None,
        );
        assert_eq!(request_target(&run.journal.entries[1]), base_face);
        assert_eq!(request_target(&run.journal.entries[2]), boss_end);
        assert_eq!(request_target(&run.journal.entries[3]), pocket_floor);
        assert_ne!(base_face.snapshot, SnapshotId::ZERO);
        assert_ne!(boss_end.snapshot, SnapshotId::ZERO);
        assert_ne!(pocket_floor.snapshot, SnapshotId::ZERO);

        let journal_json = to_pretty_json(&run.journal).unwrap();
        assert!(!journal_json.contains("target_entity"));
        assert!(!journal_json.contains("from_step"));
        let decoded = parse_journal_json(&journal_json).unwrap();
        assert_eq!(decoded, run.journal);
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert_eq!(replay.final_report, run.final_report);
    }

    #[test]
    fn history_reference_follows_one_to_one_outputs_to_the_current_snapshot() {
        let transform_case = parse_case_json(TRANSFORM_CASE).unwrap();
        let transform_run = run_case(&transform_case).unwrap();
        assert!(transform_run.passed(), "{:?}", transform_run.failures);
        let chain_case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        let step = &chain_case.steps[1];
        let reference = step.target_entity.as_ref().unwrap();

        let resolved = resolve_history_output(
            &chain_case,
            step,
            reference,
            &transform_run.journal.entries,
            success_report(&transform_run.journal.entries[1]).output_snapshot,
        )
        .unwrap();
        let original = unique_role_output(
            success_report(&transform_run.journal.entries[0]),
            EntityKind::Face,
            "face",
            Some(1),
        );
        let expected = success_report(&transform_run.journal.entries[1])
            .history
            .iter()
            .find(|record| record.inputs.as_slice() == [original])
            .and_then(|record| {
                record
                    .outputs
                    .iter()
                    .copied()
                    .find(|output| output.kind == EntityKind::Face)
            })
            .expect("the transform maps every face one-to-one");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn target_reference_validation_and_resolution_fail_closed() {
        let mut case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        case.steps[1].target_entity.as_mut().unwrap().from_step = "cut-boss-end".to_owned();
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::InvalidTargetEntityReference { message, .. })
                if message.contains("must name an earlier step")
        ));

        let mut case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        case.steps[1].target_entity.as_mut().unwrap().ordinal = None;
        assert!(matches!(
            run_case(&case),
            Err(TestkitError::TargetEntityResolution { message, .. })
                if message.contains("resolved 6")
        ));

        let mut case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        case.steps[1].target_entity.as_mut().unwrap().role = "missing.face.role".to_owned();
        assert!(matches!(
            run_case(&case),
            Err(TestkitError::TargetEntityResolution { message, .. })
                if message.contains("resolved 0")
        ));

        let mut case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        case.steps[1].target_entity.as_mut().unwrap().kind = EntityKind::Edge;
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::InvalidTargetEntityReference { message, .. })
                if message.contains("must have kind `face`")
        ));

        let mut source_case = parse_case_json(FACE_CHAIN_CASE).unwrap();
        let mut split_target_step = source_case.steps[2].clone();
        split_target_step.target_entity = source_case.steps[1].target_entity.clone();
        source_case.steps.truncate(2);
        let source_run = run_case(&source_case).unwrap();
        assert!(source_run.passed(), "{:?}", source_run.failures);
        let split_reference = split_target_step.target_entity.as_ref().unwrap();
        let resolved = resolve_history_output(
            &source_case,
            &split_target_step,
            split_reference,
            &source_run.journal.entries,
            success_report(&source_run.journal.entries[1]).output_snapshot,
        )
        .expect("a hole-aware shoulder keeps the selected source face unambiguous");
        assert_eq!(
            resolved,
            unique_role_output(
                success_report(&source_run.journal.entries[1]),
                EntityKind::Face,
                "face_extrude.target_face_patch",
                None,
            )
        );
    }

    fn success_report(entry: &JournalEntry) -> &OperationReport {
        let JournalOutcome::Success { report } = &entry.observed else {
            panic!("expected a successful journal entry")
        };
        report
    }

    fn request_target(entry: &JournalEntry) -> EntityRef {
        let KernelCommand::ExtrudeFaceProfile { target_face, .. } = &entry.request.command else {
            panic!("expected a concrete selected-face extrusion command")
        };
        *target_face
    }

    fn unique_role_output(
        report: &OperationReport,
        kind: EntityKind,
        role_name: &str,
        ordinal: Option<u32>,
    ) -> EntityRef {
        let outputs = report
            .history
            .iter()
            .filter(|record| {
                record.role.as_ref().is_some_and(|role| {
                    role.name == role_name
                        && ordinal.is_none_or(|ordinal| role.ordinal == Some(ordinal))
                })
            })
            .flat_map(|record| record.outputs.iter().copied())
            .filter(|output| output.kind == kind)
            .collect::<BTreeSet<_>>();
        assert_eq!(outputs.len(), 1, "role selector must be unique");
        *outputs.first().unwrap()
    }

    #[test]
    fn finite_request_with_overflowing_derived_measurement_still_journals_and_replays() {
        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        case.case_id = "derived-measure-overflow".to_owned();
        case.precision.max_abs_coordinate = 1.0e104;
        case.steps[0].command = KernelCommand::MakeCuboid {
            origin: Point3::default(),
            size_x: 1.0e103,
            size_y: 1.0e103,
            size_z: 1.0e103,
        };

        assert!(to_pretty_json(&case).is_ok());
        let run = run_case(&case).unwrap();
        let JournalOutcome::Error { error, .. } = &run.journal.entries[0].observed else {
            panic!("overflowing derived measures must reject rather than publish");
        };
        assert_eq!(error.code, KernelErrorCode::ValidationFailed);
        assert!(error.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .details
                .get("measurement_status")
                .map(String::as_str)
                == Some("omitted_non_finite")
        }));

        let journal_json = to_pretty_json(&run.journal).unwrap();
        let decoded = parse_journal_json(&journal_json).unwrap();
        let replay = replay_journal(&decoded).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
    }

    #[test]
    fn case_expectation_numbers_are_finite_only_when_serialized() {
        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        let ExpectedOutcome::Success { bounds, .. } = &mut case.steps[0].expected else {
            panic!("canonical case should expect success");
        };
        bounds.absolute_tolerance = f64::NAN;
        assert!(to_pretty_json(&case).is_err());

        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        let ExpectedOutcome::Success { measures, .. } = &mut case.steps[0].expected else {
            panic!("canonical case should expect success");
        };
        measures.as_mut().unwrap().surface_area = f64::INFINITY;
        assert!(to_pretty_json(&case).is_err());
    }

    #[test]
    fn expected_rejection_runs_and_replays_without_publishing_geometry() {
        let case = parse_case_json(ZERO_WIDTH_CASE).unwrap();
        let run = run_case(&case).unwrap();
        assert!(run.passed(), "{:?}", run.failures);
        assert!(run.final_report.is_none());
        assert!(run.final_scene.triangles.is_empty());
        assert!(run.final_scene.edges.is_empty());
        assert!(matches!(
            run.journal.entries[0].observed,
            JournalOutcome::Error { .. }
        ));

        let replay = replay_journal(&run.journal).unwrap();
        assert!(replay.passed(), "{:?}", replay.failures);
        assert!(replay.final_report.is_none());
    }

    #[test]
    fn one_hundred_runs_per_case_have_one_digest_and_journal() {
        for source in [CANONICAL_CASE, FACE_CHAIN_CASE, FACE_PUSH_PULL_CASE] {
            let case = parse_case_json(source).unwrap();
            let first = run_case(&case).unwrap();
            let digest = first.final_digest().unwrap();
            let journal = to_pretty_json(&first.journal).unwrap();
            for _ in 1..100 {
                let run = run_case(&case).unwrap();
                assert!(run.passed(), "{}: {:?}", case.case_id, run.failures);
                assert_eq!(run.final_digest(), Some(digest), "{}", case.case_id);
                assert_eq!(
                    to_pretty_json(&run.journal).unwrap(),
                    journal,
                    "{}",
                    case.case_id
                );
            }
        }
    }

    #[test]
    fn an_expectation_mismatch_is_a_case_failure_not_a_kernel_failure() {
        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        let ExpectedOutcome::Success { topology, .. } = &mut case.steps[0].expected else {
            panic!("canonical case should expect success");
        };
        topology.faces = 7;

        let run = run_case(&case).unwrap();
        assert!(!run.passed());
        assert!(run.failures[0].message.contains("topology mismatch"));
        assert!(matches!(
            run.journal.entries[0].observed,
            JournalOutcome::Success { .. }
        ));

        let case = parse_case_json(TRANSFORM_CASE).unwrap();
        let ExpectedOutcome::Success { history, .. } = &case.steps[1].expected else {
            panic!("transform case should expect success");
        };
        let expected_history = history.expect("transform case should pin history");
        let mut duplicate_mapping = run_case(&case).unwrap().final_report.unwrap();
        duplicate_mapping.history[1] = duplicate_mapping.history[0].clone();
        assert!(expected_history.mismatch(&duplicate_mapping).is_some());

        let mut case = parse_case_json(TRANSFORM_CASE).unwrap();
        let ExpectedOutcome::Success { history, .. } = &mut case.steps[1].expected else {
            panic!("transform case should expect success");
        };
        history.as_mut().unwrap().record_count = 57;
        let run = run_case(&case).unwrap();
        assert!(
            run.failures
                .iter()
                .any(|failure| failure.message.contains("history mismatch"))
        );
    }

    #[test]
    fn cuboid_svg_is_deterministic_and_source_mapped() {
        let case = parse_case_json(CANONICAL_CASE).unwrap();
        let run = run_case(&case).unwrap();
        let first = scene_svg(&run).unwrap();
        let second = scene_svg(&run).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, CANONICAL_SVG);
        assert_eq!(first.matches("<polygon ").count(), 12);
        assert_eq!(first.matches("<line ").count(), 12);
        assert!(first.contains("data-face="));
        assert!(first.contains("data-edge="));
        assert!(first.contains("data-role=\"positive-z\""));
    }

    #[test]
    fn version_checks_fail_closed() {
        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        case.schema_version = 99;
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::UnsupportedCaseSchema { actual: 99, .. })
        ));

        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        case.protocol_version = ProtocolVersion(0);
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::UnsupportedProtocol {
                actual: ProtocolVersion(0),
                ..
            })
        ));

        let mut case = parse_case_json(CANONICAL_CASE).unwrap();
        case.required_capabilities = vec!["native.boolean.v0".to_owned()];
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::UnsupportedCapability { .. })
        ));

        let mut case = parse_case_json(TRANSFORM_CASE).unwrap();
        case.required_capabilities
            .retain(|capability| capability != "native.transform_snapshot.v0");
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::MissingCapabilityDeclaration {
                capability,
                ..
            }) if capability == "native.transform_snapshot.v0"
        ));

        let mut case = parse_case_json(EXTRUDE_RECTANGLE_CASE).unwrap();
        case.required_capabilities.clear();
        assert!(matches!(
            validate_case(&case),
            Err(TestkitError::MissingCapabilityDeclaration {
                capability,
                ..
            }) if capability == "native.extrude_polygon.v0"
        ));

        let case = parse_case_json(CANONICAL_CASE).unwrap();
        let mut journal = run_case(&case).unwrap().journal;
        journal.protocol_version = ProtocolVersion(0);
        assert!(matches!(
            validate_journal(&journal),
            Err(TestkitError::UnsupportedProtocol {
                actual: ProtocolVersion(0),
                ..
            })
        ));

        let mut journal = run_case(&case).unwrap().journal;
        journal.entries[0].request.protocol_version = ProtocolVersion(0);
        assert!(matches!(
            validate_journal(&journal),
            Err(TestkitError::UnsupportedProtocol {
                actual: ProtocolVersion(0),
                ..
            })
        ));
    }
}

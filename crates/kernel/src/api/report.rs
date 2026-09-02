//! The machine-readable account of a session: every step with the rung that
//! certified it and whether it was exact, the finished body with its exact
//! measures and every face and edge described, the names the script gave,
//! and the first failure if the run stopped short.
//!
//! The report is what a verification-driven caller reads instead of prose.
//! Its shape is versioned by [`REPORT_SCHEMA_VERSION`] and documented in
//! `docs/report-schema.json`; adding a field is compatible, changing one is
//! a new version.

use std::collections::BTreeMap;
use std::time::Instant;

use artificer_protocol::{
    Aabb3, Diagnostic, DiagnosticSeverity, EntityKind, EntityRef, KernelStage, Point3,
    PrecisionPolicy, SemanticDigest, SnapshotId, Tier, TopologyCounts,
};
use serde::{Deserialize, Serialize};

use crate::api::commands::ApiCommand;
use crate::api::debug::{ApiError, ApiErrorCode, CommandResult};
use crate::api::scripting::{ModuleResolver, NoModules, ScriptError, compile_program_with};
use crate::api::selectors::{EntitySelector, resolve_selector};
use crate::api::session::Session;
use crate::{CancellationToken, EdgeDescription, FaceDescription, NativeKernel, SurfaceCounts};

/// The version of the report's shape. See `docs/report-schema.json`.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Whether the run reached the end of its script or command list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Failed,
}

/// Where a failure happened: compiling the script, or executing a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePhase {
    Compile,
    Execute,
}

/// Who gave an entity its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameSource {
    /// A `let name = <selector>` in the script.
    Script,
    /// The step and role that made the entity, as `step.role` or
    /// `step.role[ordinal]`.
    History,
}

/// Everything a session did, in a form a program reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionReport {
    pub schema_version: u32,
    /// The kernel crate's version.
    pub kernel_version: String,
    pub status: RunStatus,
    /// Exact when every step was; approximate when any step fell to the
    /// faceted tier.
    pub tier: Tier,
    pub precision: PrecisionPolicy,
    /// The script parameters as resolved for this run, when the session ran
    /// a script.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, f64>,
    /// Every committed step, in order.
    pub steps: Vec<StepRecord>,
    /// The failure that stopped the run, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<StepFailure>,
    /// The current body, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyReport>,
    /// Every name that resolves on the current body: the script's names
    /// first, then history names, each with what it names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<NamedEntity>,
    /// Kernel time across every step.
    pub elapsed_ms: u64,
}

/// One committed step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub label: String,
    /// The command kind: `make_box`, `drill_hole`, `fillet`, and so on.
    pub command: String,
    /// The strategy rung that certified the result. Stable, slash-separated
    /// names such as `face-feature/exact-prism` or `edge-finish/rim-blend`;
    /// a sketch has none, it builds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rung: Option<String>,
    pub tier: Tier,
    pub elapsed_ms: u64,
    pub snapshot_id: SnapshotId,
    pub digest: SemanticDigest,
    pub topology: TopologyCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Aabb3>,
    /// Exact volume of the body after this step.
    pub volume: f64,
    pub surface_area: f64,
    /// Caveats the construction attached; the faceted tier's approximation
    /// warning is the one to look for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<DiagnosticRecord>,
    /// The entities the step reported by role, as selectors can name them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityRecord>,
}

/// One diagnostic, flattened to what a reader acts on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub stage: KernelStage,
    pub message: String,
}

impl DiagnosticRecord {
    fn from_diagnostic(diagnostic: &Diagnostic) -> Self {
        Self {
            code: diagnostic.code.as_str().to_owned(),
            severity: diagnostic.severity,
            stage: diagnostic.stage,
            message: diagnostic.message.clone(),
        }
    }
}

/// One entity a step produced or reshaped, by the role it reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRecord {
    /// `role` or `role[ordinal]`, as `step.face("role")` selects it.
    pub role: String,
    pub kind: EntityKind,
    pub entity: u64,
}

/// The failure that stopped a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepFailure {
    pub phase: FailurePhase,
    /// The failing step's label; empty for a compile failure.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// The failing step's command kind; `script` for a compile failure.
    pub command: String,
    pub code: ApiErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// The kernel's own diagnostics, refusal codes included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DiagnosticRecord>,
    /// One-based line in the script, when the failure can be placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

impl StepFailure {
    /// A failure of one step of `command`.
    #[must_use]
    pub fn from_step(command: &ApiCommand, error: &ApiError, line: Option<usize>) -> Self {
        Self {
            phase: FailurePhase::Execute,
            label: command.label().to_owned(),
            command: command.kind().to_owned(),
            code: error.code,
            message: error.message.clone(),
            suggestion: error.suggestion.clone(),
            diagnostics: error
                .diagnostics
                .iter()
                .map(DiagnosticRecord::from_diagnostic)
                .collect(),
            line,
            column: None,
        }
    }

    /// A failure to compile the script at all.
    #[must_use]
    pub fn from_script(error: &ScriptError) -> Self {
        let (line, column) = error
            .location()
            .map_or((None, None), |(line, column)| (Some(line), Some(column)));
        Self {
            phase: FailurePhase::Compile,
            label: String::new(),
            command: "script".to_owned(),
            code: ApiErrorCode::ScriptError,
            message: error.message().to_owned(),
            suggestion: None,
            diagnostics: Vec::new(),
            line,
            column,
        }
    }

    /// The failure's diagnostic codes, the refusal vocabulary a caller
    /// branches on.
    #[must_use]
    pub fn codes(&self) -> Vec<&str> {
        self.diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect()
    }
}

/// The current body with exact measures and every face and edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyReport {
    pub snapshot_id: SnapshotId,
    pub digest: SemanticDigest,
    pub topology: TopologyCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Aabb3>,
    /// Exact volume from the shell integral.
    pub volume: f64,
    pub surface_area: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centroid: Option<Point3>,
    /// Faces by carrier kind. A faceted-tier body is all planes.
    pub surfaces: SurfaceCounts,
    pub tier: Tier,
    /// How many steps so far fell to the faceted tier.
    pub approximate_feature_count: u64,
    pub faces: Vec<FaceRecord>,
    pub edges: Vec<EdgeRecord>,
}

/// One face of the body with every name that reaches it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceRecord {
    #[serde(flatten)]
    pub description: FaceDescription,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// One edge of the body with every name that reaches it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeRecord {
    #[serde(flatten)]
    pub description: EdgeDescription,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// A name and what it names on the current body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedEntity {
    pub name: String,
    pub source: NameSource,
    pub kind: EntityKind,
    pub entity: EntityRef,
    pub summary: String,
}

/// What running a script into a session produced.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptOutcome {
    /// One result per committed step, in order.
    pub results: Vec<CommandResult>,
    /// The failure that stopped the run, if one did.
    pub failure: Option<StepFailure>,
    /// Wall-clock time to compile and run.
    pub elapsed_ms: u64,
}

impl ScriptOutcome {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.failure.is_none()
    }
}

impl Session {
    /// The report of everything this session has done so far.
    #[must_use]
    pub fn report(&self) -> SessionReport {
        self.report_with(None)
    }

    /// The report with the failure that stopped the run attached.
    #[must_use]
    pub fn report_with(&self, failure: Option<StepFailure>) -> SessionReport {
        let steps: Vec<StepRecord> = self
            .step_order
            .iter()
            .filter_map(|label| self.step_record(label))
            .collect();
        let tier = steps
            .iter()
            .fold(Tier::Exact, |tier, step| tier.combine(step.tier));
        let approximate_feature_count = steps
            .iter()
            .filter(|step| step.tier == Tier::Approximate)
            .count() as u64;
        let elapsed_ms = steps.iter().map(|step| step.elapsed_ms).sum();
        let names = self.named_entities();
        let body = (self.snapshot.counts().solids > 0)
            .then(|| self.body_report(tier, approximate_feature_count, &names));
        SessionReport {
            schema_version: REPORT_SCHEMA_VERSION,
            kernel_version: env!("CARGO_PKG_VERSION").to_owned(),
            status: if failure.is_some() {
                RunStatus::Failed
            } else {
                RunStatus::Ok
            },
            tier,
            precision: self.precision,
            parameters: self.parameters.clone(),
            steps,
            failure,
            body,
            names,
            elapsed_ms,
        }
    }

    /// Compiles and runs a script into this session. Every step that
    /// succeeds is committed; the first failure stops the run and comes back
    /// in the outcome rather than as an error, so the report of a failed
    /// run still shows everything built before it.
    pub fn run_script(
        &mut self,
        source: &str,
        overrides: &BTreeMap<String, f64>,
        token: &CancellationToken,
    ) -> ScriptOutcome {
        self.run_script_with(source, overrides, &NoModules, token)
    }

    /// [`Self::run_script`] with the modules the script's `use` lines name
    /// loaded through `modules`.
    pub fn run_script_with(
        &mut self,
        source: &str,
        overrides: &BTreeMap<String, f64>,
        modules: &dyn ModuleResolver,
        token: &CancellationToken,
    ) -> ScriptOutcome {
        let started = Instant::now();
        let program = match compile_program_with(source, overrides, modules) {
            Ok(program) => program,
            Err(error) => {
                return ScriptOutcome {
                    results: Vec::new(),
                    failure: Some(StepFailure::from_script(&error)),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                };
            }
        };
        self.parameters = program.parameters;
        self.names = program.names;
        let mut results = Vec::new();
        let mut failure = None;
        for command in program.commands {
            match self.execute(command.clone(), token) {
                Ok(result) => results.push(result),
                Err(error) => {
                    let line = line_of_label(source, command.label());
                    failure = Some(StepFailure::from_step(&command, &error, line));
                    break;
                }
            }
        }
        ScriptOutcome {
            results,
            failure,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
    }

    /// The tier of the body a step left behind: approximate when that step
    /// or any earlier one fell to the faceted tier.
    #[must_use]
    pub fn tier_through(&self, label: &str) -> Tier {
        let mut tier = Tier::Exact;
        for step in &self.step_order {
            if let Some(report) = self.step_reports.get(step) {
                tier = tier.combine(report.tier());
            }
            if step == label {
                break;
            }
        }
        tier
    }

    /// The tier of the current body.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.step_order
            .last()
            .map_or(Tier::Exact, |label| self.tier_through(label))
    }

    fn step_record(&self, label: &str) -> Option<StepRecord> {
        let kind = self.step_kinds.get(label).cloned().or_else(|| {
            self.journal
                .entries
                .iter()
                .find(|entry| entry.label == label)
                .map(|entry| entry.command.kind().to_owned())
        })?;
        let snapshot_id = *self.step_snapshots.get(label)?;
        let snapshot = self.snapshot_cache.get(&snapshot_id)?;
        let measures = snapshot.measures();
        let elapsed_ms = self.step_elapsed_ms.get(label).copied().unwrap_or(0);
        let report = self.step_reports.get(label);
        let entities = report.map_or_else(Vec::new, |report| {
            let mut entities = Vec::new();
            for record in &report.history {
                let Some(role) = &record.role else { continue };
                // Faces and edges are what selectors and later steps name;
                // vertices would triple the list and nothing addresses them.
                for (index, output) in record.outputs.iter().enumerate().filter(|(_, output)| {
                    matches!(output.kind, EntityKind::Face | EntityKind::Edge)
                }) {
                    let name = if let Some(ordinal) = role.ordinal {
                        format!("{}[{ordinal}]", role.name)
                    } else if record.outputs.len() > 1 {
                        format!("{}[{index}]", role.name)
                    } else {
                        role.name.clone()
                    };
                    entities.push(EntityRecord {
                        role: name,
                        kind: output.kind,
                        entity: output.entity.0,
                    });
                }
            }
            entities
        });
        Some(StepRecord {
            label: label.to_owned(),
            command: kind,
            rung: report.and_then(|report| report.rung.clone()),
            tier: report.map_or(Tier::Exact, |report| report.tier()),
            elapsed_ms,
            snapshot_id,
            digest: snapshot.semantic_digest(),
            topology: snapshot.counts(),
            bounds: measures.bounds,
            volume: measures.volume,
            surface_area: measures.surface_area,
            warnings: report.map_or_else(Vec::new, |report| {
                report
                    .warnings
                    .iter()
                    .map(DiagnosticRecord::from_diagnostic)
                    .collect()
            }),
            entities,
        })
    }

    fn body_report(
        &self,
        tier: Tier,
        approximate_feature_count: u64,
        names: &[NamedEntity],
    ) -> BodyReport {
        let snapshot = &self.snapshot;
        let measures = snapshot.measures();
        let names_of = |entity: EntityRef| -> Vec<String> {
            names
                .iter()
                .filter(|named| named.entity == entity)
                .map(|named| named.name.clone())
                .collect()
        };
        let faces = NativeKernel::faces(snapshot)
            .into_iter()
            .filter_map(|face| NativeKernel::describe_face(snapshot, face).ok())
            .map(|description| FaceRecord {
                names: names_of(description.face),
                description,
            })
            .collect();
        let edges = NativeKernel::edges(snapshot)
            .into_iter()
            .filter_map(|edge| NativeKernel::describe_edge(snapshot, edge).ok())
            .map(|description| EdgeRecord {
                names: names_of(description.edge),
                description,
            })
            .collect();
        BodyReport {
            snapshot_id: snapshot.id(),
            digest: snapshot.semantic_digest(),
            topology: snapshot.counts(),
            bounds: measures.bounds,
            volume: measures.volume,
            surface_area: measures.surface_area,
            centroid: measures.centroid,
            surfaces: NativeKernel::surface_counts(snapshot),
            tier,
            approximate_feature_count,
            faces,
            edges,
        }
    }

    /// Every name that resolves on the current body: script names first,
    /// then history names, each in name order.
    #[must_use]
    pub fn named_entities(&self) -> Vec<NamedEntity> {
        let mut named = Vec::new();
        let mut taken = std::collections::BTreeSet::new();
        for (name, selector) in &self.names {
            if let Ok(entity) = self.resolve(selector)
                && taken.insert((NameSource::Script, entity))
            {
                named.push(self.named(name.clone(), NameSource::Script, entity));
            }
        }
        for (entity, name) in self.history_names() {
            if taken.insert((NameSource::History, entity)) {
                named.push(self.named(name, NameSource::History, entity));
            }
        }
        named.sort_by(|a, b| {
            (a.source == NameSource::History)
                .cmp(&(b.source == NameSource::History))
                .then_with(|| a.name.cmp(&b.name))
        });
        named
    }

    /// The history name of every face and edge of the current body that a
    /// step claims: `step.role` or `step.role[ordinal]`, from the first
    /// step that made the entity. A step reports the entities it carried
    /// over as well as the ones it made, and an edge finish reports every
    /// face of the body under one generic role; neither of those names
    /// anything, so a rim keeps its revolve's name through every later hole.
    #[must_use]
    pub fn history_names(&self) -> BTreeMap<EntityRef, String> {
        let mut names = BTreeMap::new();
        for label in &self.step_order {
            let Some(report) = self.step_reports.get(label) else {
                continue;
            };
            let edge_finish = self
                .journal
                .entries
                .iter()
                .find(|entry| &entry.label == label)
                .is_some_and(|entry| {
                    matches!(
                        entry.command,
                        ApiCommand::Fillet { .. } | ApiCommand::Chamfer { .. }
                    )
                });
            for record in &report.history {
                let Some(role) = &record.role else { continue };
                if role.name.contains("preserved") || (edge_finish && role.name == "face") {
                    continue;
                }
                let name = match role.ordinal {
                    Some(ordinal) => format!("{label}.{}[{ordinal}]", role.name),
                    None => format!("{label}.{}", role.name),
                };
                for output in &record.outputs {
                    if output.kind != EntityKind::Face && output.kind != EntityKind::Edge {
                        continue;
                    }
                    let selector = EntitySelector::ByHistory {
                        from_step: label.as_str().into(),
                        kind: output.kind,
                        role: role.name.clone(),
                        ordinal: role.ordinal,
                    };
                    if let Ok(entity) = self.resolve(&selector) {
                        names.entry(entity).or_insert_with(|| name.clone());
                    }
                }
            }
        }
        names
    }

    /// Resolves a selector to an entity of the current body. A history
    /// selector whose entity the faceted tier rebuilt without carrying its
    /// identity forward names nothing now, and is reported as nothing.
    fn resolve(&self, selector: &EntitySelector) -> Result<EntityRef, ApiError> {
        let entity = resolve_selector(
            selector,
            &self.snapshot,
            &self.step_order,
            &self.step_reports,
        )?;
        if entity.snapshot != self.snapshot.id() {
            return Err(ApiError::new(
                ApiErrorCode::SelectorNotFound,
                "The entity is not part of the current body",
            ));
        }
        Ok(entity)
    }

    fn named(&self, name: String, source: NameSource, entity: EntityRef) -> NamedEntity {
        let summary = match entity.kind {
            EntityKind::Face => NativeKernel::describe_face(&self.snapshot, entity)
                .map(|description| description.summary)
                .unwrap_or_default(),
            EntityKind::Edge => NativeKernel::describe_edge(&self.snapshot, entity)
                .map(|description| description.summary)
                .unwrap_or_default(),
            _ => String::new(),
        };
        NamedEntity {
            name,
            source,
            kind: entity.kind,
            entity,
            summary,
        }
    }
}

/// The one-based line on which a step with `label` is declared: the first
/// line carrying `label: "<label>"`. A label scoped to a function call
/// (`call/step`) is looked up by its last segment, the label the function
/// body wrote.
#[must_use]
pub fn line_of_label(source: &str, label: &str) -> Option<usize> {
    let find = |label: &str| {
        let needle = format!("label: \"{label}\"");
        let compact = format!("label:\"{label}\"");
        source
            .lines()
            .position(|line| line.contains(&needle) || line.contains(&compact))
            .map(|index| index + 1)
    };
    find(label).or_else(|| label.rsplit('/').next().and_then(find))
}

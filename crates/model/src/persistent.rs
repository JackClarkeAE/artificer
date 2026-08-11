//! Document-owned persistent entity recipes and late kernel-command binding.
//!
//! Kernel [`EntityRef`] values are deliberately snapshot-scoped. A document
//! stores [`PersistentRef`] instead, resolves it through the current feature
//! reports, and only then constructs an executable entity-targeting command.

use std::collections::BTreeSet;

use artificer_protocol::{
    EntityKind, EntityRef, KernelCommand, OperationReport, OperationRole, SnapshotId,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::FeatureId;

/// Schema written for newly-created persistent-reference recipes.
pub const CURRENT_PERSISTENT_REF_VERSION: u32 = 1;

/// Defensive limit for recursively-qualified persistent references.
pub const MAX_PERSISTENT_LINEAGE_DEPTH: usize = 64;

/// A versioned document-layer recipe for finding one kernel entity.
///
/// `producer` and `role` identify an operation-owned result. `lineage` is an
/// optional upstream recipe: when present, only matching role records whose
/// inputs descend from that upstream entity are considered. The recipe never
/// serializes a snapshot-local [`EntityRef`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistentRef {
    pub version: u32,
    pub producer: FeatureId,
    pub role: OperationRole,
    pub kind: EntityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Box<PersistentRef>>,
}

impl PersistentRef {
    /// Creates a recipe using the current persistent-reference schema.
    #[must_use]
    pub fn new(producer: FeatureId, role: OperationRole, kind: EntityKind) -> Self {
        Self {
            version: CURRENT_PERSISTENT_REF_VERSION,
            producer,
            role,
            kind,
            lineage: None,
        }
    }

    /// Qualifies this operation role with an entity from an earlier feature.
    #[must_use]
    pub fn with_lineage(mut self, lineage: PersistentRef) -> Self {
        self.lineage = Some(Box::new(lineage));
        self
    }
}

/// Serializable replay payload for a command whose entity target is late-bound.
///
/// `command_template` retains the kernel command's non-target parameters. Its
/// embedded face command target is a serialization placeholder, not document
/// authority; [`Self::rebind`] always replaces it from `target`.
/// Keeping both values in one variant prevents the persistent recipe from
/// becoming detached from the command it must qualify.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetedKernel {
    command_template: KernelCommand,
    target: PersistentRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    additional_targets: Vec<PersistentRef>,
}

impl TargetedKernel {
    /// Creates a validated snapshot-bound face-operation replay template.
    pub fn new(
        command_template: KernelCommand,
        target: PersistentRef,
    ) -> Result<Self, TargetedKernelError> {
        let expected = target_kind_for_command(&command_template)
            .ok_or(TargetedKernelError::FaceTargetCommandRequired)?;
        if target.kind != expected {
            return Err(match expected {
                EntityKind::Edge => TargetedKernelError::EdgeTargetRequired {
                    actual: target.kind,
                },
                _ => TargetedKernelError::FaceTargetRequired {
                    actual: target.kind,
                },
            });
        }
        Ok(Self {
            command_template,
            target,
            additional_targets: Vec::new(),
        })
    }

    /// Creates a validated replay template with more than one persistent edge target.
    pub fn new_many(
        command_template: KernelCommand,
        mut targets: Vec<PersistentRef>,
    ) -> Result<Self, TargetedKernelError> {
        if targets.is_empty() {
            return Err(TargetedKernelError::TargetSetRequired);
        }
        let target = targets.remove(0);
        let result = Self {
            command_template,
            target,
            additional_targets: targets,
        };
        result.validate()?;
        Ok(result)
    }

    /// Returns the non-authoritative command template retained for replay.
    #[must_use]
    pub const fn command_template(&self) -> &KernelCommand {
        &self.command_template
    }

    /// Returns the authoritative document-level target recipe.
    #[must_use]
    pub const fn target(&self) -> &PersistentRef {
        &self.target
    }

    /// Iterates every authoritative document-level target recipe.
    pub fn targets(&self) -> impl Iterator<Item = &PersistentRef> {
        std::iter::once(&self.target).chain(self.additional_targets.iter())
    }

    /// Validates a value, including one reconstructed by deserialization.
    pub fn validate(&self) -> Result<(), TargetedKernelError> {
        let expected = target_kind_for_command(&self.command_template)
            .ok_or(TargetedKernelError::FaceTargetCommandRequired)?;
        if self.targets().any(|target| target.kind != expected) {
            return Err(match expected {
                EntityKind::Edge => TargetedKernelError::EdgeTargetRequired {
                    actual: self
                        .targets()
                        .find(|target| target.kind != expected)
                        .map_or(self.target.kind, |target| target.kind),
                },
                _ => TargetedKernelError::FaceTargetRequired {
                    actual: self
                        .targets()
                        .find(|target| target.kind != expected)
                        .map_or(self.target.kind, |target| target.kind),
                },
            });
        }
        if matches!(self.command_template, KernelCommand::FinishEdges { .. })
            && self.additional_targets.is_empty()
        {
            return Err(TargetedKernelError::TargetSetRequired);
        }
        Ok(())
    }

    /// Consumes the template and constructs its executable snapshot-bound command.
    #[must_use]
    pub fn rebind(
        self,
        reports: &[FeatureOperationReport<'_>],
        current_snapshot: SnapshotId,
    ) -> PersistentResolution<KernelCommand> {
        let Self {
            command_template,
            target,
            additional_targets,
        } = self;
        let targets = std::iter::once(target).chain(additional_targets).collect();
        rebind_target_command(command_template, targets, reports, current_snapshot)
    }
}

/// Invalid pairing of a command template and persistent entity recipe.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum TargetedKernelError {
    #[error("a persistent face target is required, received {actual:?}")]
    FaceTargetRequired { actual: EntityKind },
    #[error("the targeted command template must be a supported face-targeting command")]
    FaceTargetCommandRequired,
    #[error("a persistent edge target is required, received {actual:?}")]
    EdgeTargetRequired { actual: EntityKind },
    #[error("a non-empty compatible persistent target set is required")]
    TargetSetRequired,
}

/// One successful kernel report associated with its document feature.
///
/// Pass these in feature execution order. Reports from independent body
/// branches may coexist in the slice; resolution follows only snapshot chains
/// that are reachable from the referenced producer.
#[derive(Clone, Copy, Debug)]
pub struct FeatureOperationReport<'a> {
    pub feature: FeatureId,
    pub report: &'a OperationReport,
}

impl<'a> FeatureOperationReport<'a> {
    #[must_use]
    pub const fn new(feature: FeatureId, report: &'a OperationReport) -> Self {
        Self { feature, report }
    }
}

/// Why a persistent recipe could not produce a current entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistentMissingReason {
    UnsupportedVersion { actual: u32, supported: u32 },
    LineageDepthExceeded { limit: usize },
    ProducerReportMissing,
    RoleOutputMissing,
    LineageDoesNotMatchRole,
    NoDescendantInCurrentSnapshot { current_snapshot: SnapshotId },
    FaceTargetRequired { actual: EntityKind },
    FaceTargetCommandRequired,
    EdgeTargetRequired { actual: EntityKind },
}

/// Diagnostic context for a missing persistent entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistentMissing {
    pub producer: FeatureId,
    pub at_feature: Option<FeatureId>,
    pub reason: PersistentMissingReason,
}

/// Why resolution yielded more than one valid answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistentAmbiguityReason {
    DuplicateProducerReports,
    MultipleCandidates,
}

/// Candidate set retained when the resolver refuses to guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistentAmbiguity {
    pub producer: FeatureId,
    pub at_feature: Option<FeatureId>,
    pub reason: PersistentAmbiguityReason,
    pub candidates: Vec<EntityRef>,
}

/// Explicit result of document-level persistent-reference resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistentResolution<T> {
    Missing(PersistentMissing),
    Ambiguous(PersistentAmbiguity),
    Resolved(T),
}

impl<T> PersistentResolution<T> {
    /// Maps a successfully resolved value without collapsing diagnostics.
    #[must_use]
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> PersistentResolution<U> {
        match self {
            Self::Missing(missing) => PersistentResolution::Missing(missing),
            Self::Ambiguous(ambiguous) => PersistentResolution::Ambiguous(ambiguous),
            Self::Resolved(value) => PersistentResolution::Resolved(map(value)),
        }
    }
}

/// Resolves a document recipe to exactly one entity in `current_snapshot`.
///
/// Role outputs seed the search at the producing feature. Every subsequent
/// report whose input snapshot is reachable contributes its explicit history
/// descendants. Old numeric entity IDs are never compared across snapshots.
#[must_use]
pub fn resolve_persistent_ref(
    reference: &PersistentRef,
    reports: &[FeatureOperationReport<'_>],
    current_snapshot: SnapshotId,
) -> PersistentResolution<EntityRef> {
    let candidates =
        match resolve_candidates(reference, reports, reports.len(), current_snapshot, 0) {
            PersistentResolution::Resolved(candidates) => candidates,
            PersistentResolution::Missing(missing) => {
                return PersistentResolution::Missing(missing);
            }
            PersistentResolution::Ambiguous(ambiguous) => {
                return PersistentResolution::Ambiguous(ambiguous);
            }
        };

    match candidates.len() {
        0 => PersistentResolution::Missing(PersistentMissing {
            producer: reference.producer,
            at_feature: None,
            reason: PersistentMissingReason::NoDescendantInCurrentSnapshot { current_snapshot },
        }),
        1 => PersistentResolution::Resolved(
            *candidates
                .first()
                .expect("a singleton candidate set has one entity"),
        ),
        _ => PersistentResolution::Ambiguous(PersistentAmbiguity {
            producer: reference.producer,
            at_feature: None,
            reason: PersistentAmbiguityReason::MultipleCandidates,
            candidates: candidates.into_iter().collect(),
        }),
    }
}

/// Consumes a serialized face-operation command template and late-binds its target.
///
/// The raw `target_face` contained in `command` is intentionally ignored. A
/// caller should send only the command returned by the [`PersistentResolution::Resolved`]
/// branch to the kernel. Missing or ambiguous resolution consumes and drops
/// the template, preventing accidental execution against its stale raw ID.
#[must_use]
pub fn rebind_face_target_command(
    command: KernelCommand,
    target: &PersistentRef,
    reports: &[FeatureOperationReport<'_>],
    current_snapshot: SnapshotId,
) -> PersistentResolution<KernelCommand> {
    rebind_target_command(command, vec![target.clone()], reports, current_snapshot)
}

fn rebind_target_command(
    mut command: KernelCommand,
    targets: Vec<PersistentRef>,
    reports: &[FeatureOperationReport<'_>],
    current_snapshot: SnapshotId,
) -> PersistentResolution<KernelCommand> {
    let first = targets
        .first()
        .expect("validated targeted commands always carry at least one target");
    let Some(expected_kind) = target_kind_for_command(&command) else {
        return PersistentResolution::Missing(PersistentMissing {
            producer: first.producer,
            at_feature: None,
            reason: PersistentMissingReason::FaceTargetCommandRequired,
        });
    };
    if let Some(target) = targets.iter().find(|target| target.kind != expected_kind) {
        return PersistentResolution::Missing(PersistentMissing {
            producer: target.producer,
            at_feature: None,
            reason: if expected_kind == EntityKind::Edge {
                PersistentMissingReason::EdgeTargetRequired {
                    actual: target.kind,
                }
            } else {
                PersistentMissingReason::FaceTargetRequired {
                    actual: target.kind,
                }
            },
        });
    }

    let mut resolved_targets = Vec::with_capacity(targets.len());
    for target in &targets {
        match resolve_persistent_ref(target, reports, current_snapshot) {
            PersistentResolution::Resolved(resolved) => resolved_targets.push(resolved),
            PersistentResolution::Missing(missing) => {
                return PersistentResolution::Missing(missing);
            }
            PersistentResolution::Ambiguous(ambiguous) => {
                return PersistentResolution::Ambiguous(ambiguous);
            }
        }
    }
    match resolved_targets.as_slice() {
        [] => unreachable!("the target set was checked above"),
        [resolved] => {
            match &mut command {
                KernelCommand::ExtrudeFaceProfile { target_face, .. }
                | KernelCommand::ExtrudeFacePlanarProfile { target_face, .. }
                | KernelCommand::PushPullFace { target_face, .. }
                | KernelCommand::DrillHole { target_face, .. }
                | KernelCommand::AddRib { target_face, .. } => *target_face = *resolved,
                KernelCommand::FinishEdge { target_edge, .. } => *target_edge = *resolved,
                KernelCommand::FinishEdges { target_edges, .. } => *target_edges = vec![*resolved],
                _ => unreachable!("the command variant was checked above"),
            }
            PersistentResolution::Resolved(command)
        }
        resolved => {
            if let KernelCommand::FinishEdges { target_edges, .. } = &mut command {
                *target_edges = resolved.to_vec();
                PersistentResolution::Resolved(command)
            } else {
                PersistentResolution::Missing(PersistentMissing {
                    producer: first.producer,
                    at_feature: None,
                    reason: PersistentMissingReason::FaceTargetCommandRequired,
                })
            }
        }
    }
}

/// Backward-compatible name retained for callers that only construct
/// `ExtrudeFaceProfile`; new code should use [`rebind_face_target_command`].
#[must_use]
pub fn rebind_extrude_face_profile(
    command: KernelCommand,
    target: &PersistentRef,
    reports: &[FeatureOperationReport<'_>],
    current_snapshot: SnapshotId,
) -> PersistentResolution<KernelCommand> {
    rebind_face_target_command(command, target, reports, current_snapshot)
}

const fn target_kind_for_command(command: &KernelCommand) -> Option<EntityKind> {
    match command {
        KernelCommand::ExtrudeFaceProfile { .. }
        | KernelCommand::ExtrudeFacePlanarProfile { .. }
        | KernelCommand::PushPullFace { .. }
        | KernelCommand::DrillHole { .. }
        | KernelCommand::AddRib { .. } => Some(EntityKind::Face),
        KernelCommand::FinishEdge { .. } | KernelCommand::FinishEdges { .. } => {
            Some(EntityKind::Edge)
        }
        _ => None,
    }
}

fn resolve_candidates(
    reference: &PersistentRef,
    reports: &[FeatureOperationReport<'_>],
    end_exclusive: usize,
    target_snapshot: SnapshotId,
    depth: usize,
) -> PersistentResolution<BTreeSet<EntityRef>> {
    if reference.version != CURRENT_PERSISTENT_REF_VERSION {
        return PersistentResolution::Missing(PersistentMissing {
            producer: reference.producer,
            at_feature: None,
            reason: PersistentMissingReason::UnsupportedVersion {
                actual: reference.version,
                supported: CURRENT_PERSISTENT_REF_VERSION,
            },
        });
    }
    if depth >= MAX_PERSISTENT_LINEAGE_DEPTH {
        return PersistentResolution::Missing(PersistentMissing {
            producer: reference.producer,
            at_feature: None,
            reason: PersistentMissingReason::LineageDepthExceeded {
                limit: MAX_PERSISTENT_LINEAGE_DEPTH,
            },
        });
    }

    let producer_indices = reports[..end_exclusive]
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| (operation.feature == reference.producer).then_some(index))
        .collect::<Vec<_>>();
    let producer_index = match producer_indices.as_slice() {
        [] => {
            return PersistentResolution::Missing(PersistentMissing {
                producer: reference.producer,
                at_feature: None,
                reason: PersistentMissingReason::ProducerReportMissing,
            });
        }
        [index] => *index,
        _ => {
            return PersistentResolution::Ambiguous(PersistentAmbiguity {
                producer: reference.producer,
                at_feature: Some(reference.producer),
                reason: PersistentAmbiguityReason::DuplicateProducerReports,
                candidates: Vec::new(),
            });
        }
    };
    let producer = reports[producer_index];

    let lineage_candidates = if let Some(lineage) = &reference.lineage {
        match resolve_candidates(
            lineage,
            reports,
            producer_index,
            producer.report.input_snapshot,
            depth + 1,
        ) {
            PersistentResolution::Resolved(candidates) => Some(candidates),
            PersistentResolution::Missing(missing) => {
                return PersistentResolution::Missing(missing);
            }
            PersistentResolution::Ambiguous(ambiguous) => {
                return PersistentResolution::Ambiguous(ambiguous);
            }
        }
    } else {
        None
    };

    let matching_records = producer.report.history.iter().filter(|record| {
        record.role.as_ref() == Some(&reference.role)
            && lineage_candidates
                .as_ref()
                .is_none_or(|lineage| record.inputs.iter().any(|input| lineage.contains(input)))
    });
    let mut candidates = matching_records
        .flat_map(|record| record.outputs.iter().copied())
        .filter(|output| {
            output.kind == reference.kind && output.snapshot == producer.report.output_snapshot
        })
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return PersistentResolution::Missing(PersistentMissing {
            producer: reference.producer,
            at_feature: Some(reference.producer),
            reason: if lineage_candidates.is_some() {
                PersistentMissingReason::LineageDoesNotMatchRole
            } else {
                PersistentMissingReason::RoleOutputMissing
            },
        });
    }

    // Keep old-snapshot candidates while exploring. This permits independent
    // body branches and report DAGs: only candidates in `target_snapshot` are
    // selected below, while every reachable report can add descendants.
    for operation in &reports[producer_index + 1..end_exclusive] {
        let reachable_inputs = candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.snapshot == operation.report.input_snapshot)
            .collect::<BTreeSet<_>>();
        if reachable_inputs.is_empty() {
            continue;
        }
        candidates.extend(
            operation
                .report
                .history
                .iter()
                .filter(|record| {
                    record
                        .inputs
                        .iter()
                        .any(|input| reachable_inputs.contains(input))
                })
                .flat_map(|record| record.outputs.iter().copied())
                .filter(|output| {
                    output.kind == reference.kind
                        && output.snapshot == operation.report.output_snapshot
                }),
        );
    }

    let current = candidates
        .into_iter()
        .filter(|candidate| candidate.snapshot == target_snapshot)
        .collect::<BTreeSet<_>>();
    if current.is_empty() {
        PersistentResolution::Missing(PersistentMissing {
            producer: reference.producer,
            at_feature: None,
            reason: PersistentMissingReason::NoDescendantInCurrentSnapshot {
                current_snapshot: target_snapshot,
            },
        })
    } else {
        PersistentResolution::Resolved(current)
    }
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        EdgeFinishKind, EntityId, FaceExtrusionOperation, HistoryRecord, HistoryRelation,
        PlanarFrame3, PlanarProfile2, Point2, Point3, SemanticDigest, TopologyCounts,
        ValidationProfile, ValidationReport, Vector3,
    };

    use super::*;

    fn feature(value: u64) -> FeatureId {
        FeatureId::from_allocated(value)
    }

    fn snapshot(byte: u8) -> SnapshotId {
        SnapshotId::new([byte; 16])
    }

    fn entity(snapshot: SnapshotId, id: u64, kind: EntityKind) -> EntityRef {
        EntityRef {
            snapshot,
            entity: EntityId(id),
            kind,
        }
    }

    fn record(
        relation: HistoryRelation,
        inputs: Vec<EntityRef>,
        outputs: Vec<EntityRef>,
        role: &str,
        ordinal: Option<u32>,
    ) -> HistoryRecord {
        HistoryRecord {
            relation,
            inputs,
            outputs,
            role: Some(OperationRole::new(role, ordinal)),
        }
    }

    fn report(input: u8, output: u8, history: Vec<HistoryRecord>) -> OperationReport {
        OperationReport {
            input_snapshot: snapshot(input),
            output_snapshot: snapshot(output),
            semantic_digest: SemanticDigest::new([output; 32]),
            topology: TopologyCounts::default(),
            bounds: None,
            history,
            validation: ValidationReport {
                profile: ValidationProfile::Topology,
                valid: true,
                diagnostics: Vec::new(),
            },
            warnings: Vec::new(),
        }
    }

    fn face_ref(producer: u64, role: &str, ordinal: Option<u32>) -> PersistentRef {
        PersistentRef::new(
            feature(producer),
            OperationRole::new(role, ordinal),
            EntityKind::Face,
        )
    }

    #[test]
    fn recipe_round_trip_contains_intent_but_no_raw_entity_reference() {
        let reference = face_ref(2, "face_extrude.boss.side_face", Some(3)).with_lineage(face_ref(
            1,
            "extrude.top_face",
            None,
        ));

        let value = serde_json::to_value(&reference).expect("recipe should serialize");
        assert_eq!(value["version"], CURRENT_PERSISTENT_REF_VERSION);
        assert_eq!(value["producer"], 2);
        assert_eq!(value["role"]["ordinal"], 3);
        assert!(value.get("snapshot").is_none());
        assert!(value.get("entity").is_none());
        assert_eq!(
            serde_json::from_value::<PersistentRef>(value).unwrap(),
            reference
        );
    }

    #[test]
    fn role_seed_follows_explicit_history_to_current_snapshot() {
        let made = entity(snapshot(1), 11, EntityKind::Face);
        let rebuilt = entity(snapshot(2), 91, EntityKind::Face);
        let base = report(
            0,
            1,
            vec![record(
                HistoryRelation::Generated,
                Vec::new(),
                vec![made],
                "extrude.top_face",
                None,
            )],
        );
        let edit = report(
            1,
            2,
            vec![record(
                HistoryRelation::Unchanged,
                vec![made],
                vec![rebuilt],
                "face_extrude.preserved_face",
                Some(0),
            )],
        );
        let reports = [
            FeatureOperationReport::new(feature(1), &base),
            FeatureOperationReport::new(feature(2), &edit),
        ];

        assert_eq!(
            resolve_persistent_ref(
                &face_ref(1, "extrude.top_face", None),
                &reports,
                snapshot(2)
            ),
            PersistentResolution::Resolved(rebuilt)
        );
    }

    #[test]
    fn lineage_qualifies_identically_named_role_outputs() {
        let left = entity(snapshot(1), 1, EntityKind::Face);
        let right = entity(snapshot(1), 2, EntityKind::Face);
        let left_after = entity(snapshot(2), 20, EntityKind::Face);
        let right_after = entity(snapshot(2), 21, EntityKind::Face);
        let base = report(
            0,
            1,
            vec![
                record(
                    HistoryRelation::Generated,
                    Vec::new(),
                    vec![left],
                    "base.face",
                    Some(0),
                ),
                record(
                    HistoryRelation::Generated,
                    Vec::new(),
                    vec![right],
                    "base.face",
                    Some(1),
                ),
            ],
        );
        let edit = report(
            1,
            2,
            vec![
                record(
                    HistoryRelation::Unchanged,
                    vec![left],
                    vec![left_after],
                    "preserved.face",
                    None,
                ),
                record(
                    HistoryRelation::Unchanged,
                    vec![right],
                    vec![right_after],
                    "preserved.face",
                    None,
                ),
            ],
        );
        let reports = [
            FeatureOperationReport::new(feature(1), &base),
            FeatureOperationReport::new(feature(2), &edit),
        ];
        let qualified =
            face_ref(2, "preserved.face", None).with_lineage(face_ref(1, "base.face", Some(1)));

        assert_eq!(
            resolve_persistent_ref(&qualified, &reports, snapshot(2)),
            PersistentResolution::Resolved(right_after)
        );
    }

    #[test]
    fn split_history_is_ambiguous_instead_of_guessing() {
        let source = entity(snapshot(1), 1, EntityKind::Face);
        let first = entity(snapshot(2), 2, EntityKind::Face);
        let second = entity(snapshot(2), 3, EntityKind::Face);
        let base = report(
            0,
            1,
            vec![record(
                HistoryRelation::Generated,
                Vec::new(),
                vec![source],
                "base.face",
                None,
            )],
        );
        let split = report(
            1,
            2,
            vec![record(
                HistoryRelation::Modified,
                vec![source],
                vec![first, second],
                "split.face",
                None,
            )],
        );
        let reports = [
            FeatureOperationReport::new(feature(1), &base),
            FeatureOperationReport::new(feature(2), &split),
        ];

        assert_eq!(
            resolve_persistent_ref(&face_ref(1, "base.face", None), &reports, snapshot(2)),
            PersistentResolution::Ambiguous(PersistentAmbiguity {
                producer: feature(1),
                at_feature: None,
                reason: PersistentAmbiguityReason::MultipleCandidates,
                candidates: vec![first, second],
            })
        );
    }

    #[test]
    fn split_shell_and_solid_history_are_ambiguous_instead_of_choosing_a_fragment() {
        for kind in [EntityKind::Shell, EntityKind::Solid] {
            let source = entity(snapshot(1), 1, kind);
            let first = entity(snapshot(2), 2, kind);
            let second = entity(snapshot(2), 3, kind);
            let base = report(
                0,
                1,
                vec![record(
                    HistoryRelation::Generated,
                    Vec::new(),
                    vec![source],
                    "base.entity",
                    None,
                )],
            );
            let split = report(
                1,
                2,
                vec![record(
                    HistoryRelation::Modified,
                    vec![source],
                    vec![first, second],
                    "face_extrude.split_entity",
                    None,
                )],
            );
            let reports = [
                FeatureOperationReport::new(feature(1), &base),
                FeatureOperationReport::new(feature(2), &split),
            ];
            let reference =
                PersistentRef::new(feature(1), OperationRole::new("base.entity", None), kind);

            assert_eq!(
                resolve_persistent_ref(&reference, &reports, snapshot(2)),
                PersistentResolution::Ambiguous(PersistentAmbiguity {
                    producer: feature(1),
                    at_feature: None,
                    reason: PersistentAmbiguityReason::MultipleCandidates,
                    candidates: vec![first, second],
                }),
                "a split {kind:?} must require an explicit downstream choice"
            );
        }
    }

    #[test]
    fn deleted_entity_is_structured_missing() {
        let source = entity(snapshot(1), 1, EntityKind::Face);
        let base = report(
            0,
            1,
            vec![record(
                HistoryRelation::Generated,
                Vec::new(),
                vec![source],
                "base.face",
                None,
            )],
        );
        let delete = report(
            1,
            2,
            vec![record(
                HistoryRelation::Deleted,
                vec![source],
                Vec::new(),
                "delete.face",
                None,
            )],
        );
        let reports = [
            FeatureOperationReport::new(feature(1), &base),
            FeatureOperationReport::new(feature(2), &delete),
        ];

        assert!(matches!(
            resolve_persistent_ref(&face_ref(1, "base.face", None), &reports, snapshot(2)),
            PersistentResolution::Missing(PersistentMissing {
                reason: PersistentMissingReason::NoDescendantInCurrentSnapshot { .. },
                ..
            })
        ));
    }

    #[test]
    fn rebind_discards_serialized_raw_target_and_installs_resolved_face() {
        let resolved = entity(snapshot(2), 44, EntityKind::Face);
        let source = report(
            1,
            2,
            vec![record(
                HistoryRelation::Generated,
                Vec::new(),
                vec![resolved],
                "face_extrude.boss.end_face",
                None,
            )],
        );
        let reports = [FeatureOperationReport::new(feature(7), &source)];
        let stale_raw_target = entity(snapshot(99), 999, EntityKind::Face);
        let command = KernelCommand::ExtrudeFaceProfile {
            target_face: stale_raw_target,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            vertices: vec![
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 0.0),
                Point2::new(1.0, 1.0),
            ],
            distance: 1.0,
            operation: FaceExtrusionOperation::Add,
        };

        let PersistentResolution::Resolved(KernelCommand::ExtrudeFaceProfile {
            target_face, ..
        }) = rebind_extrude_face_profile(
            command,
            &face_ref(7, "face_extrude.boss.end_face", None),
            &reports,
            snapshot(2),
        )
        else {
            panic!("the command should resolve and remain a face extrusion")
        };
        assert_eq!(target_face, resolved);
        assert_ne!(target_face, stale_raw_target);
    }

    #[test]
    fn push_pull_uses_the_same_persistent_face_resolution_boundary() {
        let resolved = entity(snapshot(4), 72, EntityKind::Face);
        let source = report(
            3,
            4,
            vec![record(
                HistoryRelation::Modified,
                vec![entity(snapshot(3), 72, EntityKind::Face)],
                vec![resolved],
                "face_push_pull.target_face",
                None,
            )],
        );
        let reports = [FeatureOperationReport::new(feature(9), &source)];
        let stale = entity(snapshot(99), 999, EntityKind::Face);
        let targeted = TargetedKernel::new(
            KernelCommand::PushPullFace {
                target_face: stale,
                distance: -1.25,
            },
            face_ref(9, "face_push_pull.target_face", None),
        )
        .expect("push/pull is a supported persistent face command");

        let PersistentResolution::Resolved(KernelCommand::PushPullFace {
            target_face,
            distance,
        }) = targeted.rebind(&reports, snapshot(4))
        else {
            panic!("push/pull should resolve through the persistent face recipe")
        };
        assert_eq!(target_face, resolved);
        assert_eq!(distance, -1.25);
    }

    #[test]
    fn edge_finish_rebinds_only_through_a_persistent_edge_recipe() {
        let resolved = entity(snapshot(4), 81, EntityKind::Edge);
        let source = report(
            3,
            4,
            vec![record(
                HistoryRelation::Modified,
                vec![entity(snapshot(3), 81, EntityKind::Edge)],
                vec![resolved],
                "edge_finish.target_edge",
                None,
            )],
        );
        let reports = [FeatureOperationReport::new(feature(10), &source)];
        let stale = entity(snapshot(99), 999, EntityKind::Edge);
        let targeted = TargetedKernel::new(
            KernelCommand::FinishEdge {
                target_edge: stale,
                kind: EdgeFinishKind::Fillet,
                distance: 0.4,
            },
            PersistentRef::new(
                feature(10),
                OperationRole::new("edge_finish.target_edge", None),
                EntityKind::Edge,
            ),
        )
        .expect("edge finish accepts a persistent edge recipe");

        let PersistentResolution::Resolved(KernelCommand::FinishEdge {
            target_edge,
            kind,
            distance,
        }) = targeted.rebind(&reports, snapshot(4))
        else {
            panic!("edge finish should resolve through persistent edge history")
        };
        assert_eq!(target_edge, resolved);
        assert_eq!(kind, EdgeFinishKind::Fillet);
        assert_eq!(distance, 0.4);
    }

    #[test]
    fn planar_region_face_feature_uses_persistent_target_rebinding() {
        let resolved = entity(snapshot(6), 91, EntityKind::Face);
        let source = report(
            5,
            6,
            vec![record(
                HistoryRelation::Generated,
                Vec::new(),
                vec![resolved],
                "face_extrude.boss.end_face",
                None,
            )],
        );
        let reports = [FeatureOperationReport::new(feature(12), &source)];
        let stale = entity(snapshot(99), 999, EntityKind::Face);
        let command = KernelCommand::ExtrudeFacePlanarProfile {
            target_face: stale,
            frame: PlanarFrame3::new(
                Point3::default(),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2::from_polygon(&[
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 0.0),
                Point2::new(1.0, 1.0),
                Point2::new(0.0, 1.0),
            ]),
            distance: 1.0,
            operation: FaceExtrusionOperation::Add,
        };

        let PersistentResolution::Resolved(KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            ..
        }) = rebind_face_target_command(
            command,
            &face_ref(12, "face_extrude.boss.end_face", None),
            &reports,
            snapshot(6),
        )
        else {
            panic!("planar face feature should use the common persistent target boundary")
        };
        assert_eq!(target_face, resolved);
        assert_ne!(target_face, stale);
    }

    #[test]
    fn missing_recipe_never_returns_the_stale_command_template() {
        let command = KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 1.0,
            size_y: 1.0,
            size_z: 1.0,
        };

        assert!(matches!(
            rebind_extrude_face_profile(command, &face_ref(1, "missing", None), &[], snapshot(1),),
            PersistentResolution::Missing(PersistentMissing {
                reason: PersistentMissingReason::FaceTargetCommandRequired,
                ..
            })
        ));
    }
}

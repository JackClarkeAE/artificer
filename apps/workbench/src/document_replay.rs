//! Fresh-process reconstruction of a persisted model document.
//!
//! Loading is deliberately staged: every immutable snapshot and operation
//! report is owned by [`HydratedDocument`] until the caller elects to publish
//! the result into the workbench. An error drops the entire stage, so a failed
//! replay cannot partially replace the currently displayed model.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_model::persistent::{
    FeatureOperationReport, PersistentAmbiguity, PersistentMissing, PersistentResolution,
};
use artificer_model::{
    BodyId, ComponentInstanceId, FeatureId, FeatureInput, FeatureNode, FeatureOutput,
    ModelDocument, ParameterOverrides, RebuildState, ReplayAction, SketchRecord,
    SketchRegionResolveError, SnapshotAssociation,
};
use artificer_protocol::{
    BooleanRequest, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelError, OperationReport,
    PrecisionPolicy, RequestId, SnapshotId,
};

/// Root precision used when reconstructing a body from the empty snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HydrationOptions {
    pub root_precision: PrecisionPolicy,
}

/// Provenance handling for one successfully replayed feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydratedProvenance {
    /// A clean persisted association exactly matched the regenerated result.
    Verified,
    /// Dirty recipes are regenerated but their stale cache association is not
    /// treated as authority.
    Dirty {
        persisted: Option<SnapshotAssociation>,
    },
}

/// One feature result in document-timeline order.
#[derive(Clone, Debug)]
pub struct HydratedFeature {
    pub feature: FeatureId,
    pub branches: Vec<BodyId>,
    pub association: SnapshotAssociation,
    pub report: Option<OperationReport>,
    pub provenance: HydratedProvenance,
}

/// Why an active feature produced no runtime result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrationSkipReason {
    ExplicitSuppression,
    SuppressedDependency(FeatureId),
    SuppressedComponent(ComponentInstanceId),
}

/// One active but deliberately omitted feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HydratedSkip {
    pub feature: FeatureId,
    pub reason: HydrationSkipReason,
}

/// Fully staged native document runtime.
///
/// `snapshots` includes the canonical empty snapshot and every unique snapshot
/// produced during replay. `branch_heads` identifies the currently evaluated
/// immutable result for each body, after suppression and the history cursor
/// have been applied.
#[derive(Clone, Debug)]
pub struct HydratedDocument {
    pub document: ModelDocument,
    pub snapshots: BTreeMap<SnapshotId, Snapshot>,
    pub features: Vec<HydratedFeature>,
    pub branch_heads: BTreeMap<BodyId, SnapshotId>,
    pub skipped: Vec<HydratedSkip>,
    pub beyond_history_cursor: Vec<FeatureId>,
}

impl HydratedDocument {
    /// Returns the retained evaluated snapshot for one logical body.
    #[must_use]
    pub fn branch_snapshot(&self, body: BodyId) -> Option<&Snapshot> {
        self.branch_heads
            .get(&body)
            .and_then(|snapshot| self.snapshots.get(snapshot))
    }

    /// Returns the regenerated operation report for one kernel feature.
    #[must_use]
    pub fn operation_report(&self, feature: FeatureId) -> Option<&OperationReport> {
        self.features
            .iter()
            .find(|result| result.feature == feature)
            .and_then(|result| result.report.as_ref())
    }
}

/// Failure to construct a complete staged runtime.
#[derive(Clone, Debug, PartialEq)]
pub enum DocumentHydrationError {
    Deserialize(String),
    ParameterEvaluation(String),
    ParameterizedAction {
        feature: FeatureId,
        message: String,
    },
    SketchRegion {
        feature: FeatureId,
        error: SketchRegionResolveError,
    },
    KernelActionWithoutBody {
        feature: FeatureId,
    },
    MissingBranchSnapshot {
        feature: FeatureId,
        body: BodyId,
    },
    MixedRootAndExistingBranches {
        feature: FeatureId,
    },
    DivergentBranchInputs {
        feature: FeatureId,
        snapshots: Vec<SnapshotId>,
    },
    SnapshotUnavailable {
        feature: FeatureId,
        snapshot: SnapshotId,
    },
    PersistentTargetMissing {
        feature: FeatureId,
        missing: PersistentMissing,
    },
    PersistentTargetAmbiguous {
        feature: FeatureId,
        ambiguity: PersistentAmbiguity,
    },
    Kernel {
        feature: FeatureId,
        error: KernelError,
    },
    MissingCleanProvenance {
        feature: FeatureId,
    },
    ProvenanceMismatch {
        feature: FeatureId,
        persisted: Box<SnapshotAssociation>,
        replayed: Box<SnapshotAssociation>,
    },
}

impl fmt::Display for DocumentHydrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deserialize(message) => {
                write!(formatter, "native document is invalid: {message}")
            }
            Self::ParameterEvaluation(message) => {
                write!(
                    formatter,
                    "native document parameters could not be evaluated: {message}"
                )
            }
            Self::ParameterizedAction { feature, message } => write!(
                formatter,
                "feature {feature} parameter binding could not be resolved: {message}"
            ),
            Self::SketchRegion { feature, error } => {
                write!(
                    formatter,
                    "feature {feature} sketch region needs repair: {error}"
                )
            }
            Self::KernelActionWithoutBody { feature } => {
                write!(formatter, "kernel feature {feature} has no body branch")
            }
            Self::MissingBranchSnapshot { feature, body } => write!(
                formatter,
                "feature {feature} cannot replay because body {body} has no staged snapshot"
            ),
            Self::MixedRootAndExistingBranches { feature } => write!(
                formatter,
                "feature {feature} mixes new and existing body branches"
            ),
            Self::DivergentBranchInputs { feature, .. } => write!(
                formatter,
                "feature {feature} targets body branches with different snapshots"
            ),
            Self::SnapshotUnavailable { feature, snapshot } => write!(
                formatter,
                "feature {feature} references unavailable snapshot {snapshot}"
            ),
            Self::PersistentTargetMissing { feature, missing } => write!(
                formatter,
                "feature {feature} persistent target is missing: {:?}",
                missing.reason
            ),
            Self::PersistentTargetAmbiguous { feature, ambiguity } => write!(
                formatter,
                "feature {feature} persistent target is ambiguous: {:?}",
                ambiguity.reason
            ),
            Self::Kernel { feature, error } => {
                write!(formatter, "kernel replay failed for {feature}: {error}")
            }
            Self::MissingCleanProvenance { feature } => {
                write!(
                    formatter,
                    "clean feature {feature} has no persisted provenance"
                )
            }
            Self::ProvenanceMismatch { feature, .. } => {
                write!(
                    formatter,
                    "clean feature {feature} failed provenance verification"
                )
            }
        }
    }
}

impl std::error::Error for DocumentHydrationError {}

/// Deserializes and reconstructs a persisted native document using the default
/// root precision policy.
pub fn hydrate_document_json(json: &str) -> Result<HydratedDocument, DocumentHydrationError> {
    hydrate_document_json_with_options(json, HydrationOptions::default())
}

/// Deserializes and reconstructs a persisted native document using an explicit
/// root precision policy.
pub fn hydrate_document_json_with_options(
    json: &str,
    options: HydrationOptions,
) -> Result<HydratedDocument, DocumentHydrationError> {
    let document = serde_json::from_str::<ModelDocument>(json)
        .map_err(|error| DocumentHydrationError::Deserialize(error.to_string()))?;
    hydrate_model_document(document, options)
}

/// Reconstructs a previously validated document into an unpublished runtime.
pub fn hydrate_model_document(
    document: ModelDocument,
    options: HydrationOptions,
) -> Result<HydratedDocument, DocumentHydrationError> {
    let empty = NativeKernel::empty();
    let mut snapshots = BTreeMap::from([(empty.id(), empty)]);
    let mut features = Vec::<HydratedFeature>::new();
    let mut branch_heads = BTreeMap::<BodyId, SnapshotId>::new();
    let mut skipped = Vec::<HydratedSkip>::new();
    let mut unavailable = BTreeSet::<FeatureId>::new();
    let active_count = document.history_position();
    let beyond_history_cursor = document.features()[active_count..]
        .iter()
        .map(|feature| feature.id)
        .collect::<Vec<_>>();
    let component_by_feature = document
        .component_instances()
        .iter()
        .map(|component| (component.created_by, (component.id, component.suppressed)))
        .collect::<BTreeMap<_, _>>();
    let evaluated_parameters = document
        .active_features()
        .iter()
        .any(|feature| matches!(feature.action, ReplayAction::ParameterizedKernel(_)))
        .then(|| document.evaluate_parameters(&ParameterOverrides::default()))
        .transpose()
        .map_err(|error| DocumentHydrationError::ParameterEvaluation(error.to_string()))?;

    for feature in document.active_features() {
        let skip_reason = if feature.state.suppressed {
            Some(HydrationSkipReason::ExplicitSuppression)
        } else if let Some(dependency) = feature
            .dependencies
            .iter()
            .copied()
            .find(|dependency| unavailable.contains(dependency))
        {
            Some(HydrationSkipReason::SuppressedDependency(dependency))
        } else if let Some((component, true)) = component_by_feature.get(&feature.id).copied() {
            Some(HydrationSkipReason::SuppressedComponent(component))
        } else {
            None
        };
        if let Some(reason) = skip_reason {
            unavailable.insert(feature.id);
            skipped.push(HydratedSkip {
                feature: feature.id,
                reason,
            });
            continue;
        }

        let branches = feature_branches(&document, feature);
        let input_id = replay_input(
            &document,
            feature,
            &branches,
            &branch_heads,
            &snapshots,
            features.last(),
        )?;
        let input =
            snapshots
                .get(&input_id)
                .ok_or(DocumentHydrationError::SnapshotUnavailable {
                    feature: feature.id,
                    snapshot: input_id,
                })?;

        let action = match &feature.action {
            ReplayAction::ParameterizedKernel(_) => feature
                .action
                .resolve_parameters(
                    evaluated_parameters
                        .as_ref()
                        .expect("parameterized actions require evaluated document parameters"),
                )
                .map_err(|error| DocumentHydrationError::ParameterizedAction {
                    feature: feature.id,
                    message: error.to_string(),
                })?,
            _ => feature.action.clone(),
        };
        let action = action
            .resolve_sketch_regions(
                &document,
                input.precision_policy().unwrap_or(options.root_precision),
            )
            .map_err(|error| DocumentHydrationError::SketchRegion {
                feature: feature.id,
                error,
            })?;
        let (association, report, output_snapshot) = match action {
            ReplayAction::Marker => (
                SnapshotAssociation::new(input.id(), input.id(), input.semantic_digest()),
                None,
                None,
            ),
            ReplayAction::Kernel(command) => {
                let outcome = execute_feature(feature.id, input, command, options.root_precision)?;
                let association = association_from_report(&outcome.report);
                (association, Some(outcome.report), Some(outcome.snapshot))
            }
            ReplayAction::TargetedKernel(targeted) => {
                let ordered_reports = features
                    .iter()
                    .filter_map(|result| {
                        result
                            .report
                            .as_ref()
                            .map(|report| FeatureOperationReport::new(result.feature, report))
                    })
                    .collect::<Vec<_>>();
                let command = match targeted.rebind(&ordered_reports, input.id()) {
                    PersistentResolution::Resolved(command) => command,
                    PersistentResolution::Missing(missing) => {
                        return Err(DocumentHydrationError::PersistentTargetMissing {
                            feature: feature.id,
                            missing,
                        });
                    }
                    PersistentResolution::Ambiguous(ambiguity) => {
                        return Err(DocumentHydrationError::PersistentTargetAmbiguous {
                            feature: feature.id,
                            ambiguity,
                        });
                    }
                };
                let outcome = execute_feature(feature.id, input, command, options.root_precision)?;
                let association = association_from_report(&outcome.report);
                (association, Some(outcome.report), Some(outcome.snapshot))
            }
            ReplayAction::ParameterizedKernel(_) => {
                unreachable!("parameterized replay actions are resolved before kernel dispatch")
            }
            ReplayAction::SketchRegionExtrusion(_) => {
                unreachable!("sketch-region replay actions are resolved before kernel dispatch")
            }
            ReplayAction::Boolean(recipe) => {
                let tool_id = branch_heads.get(&recipe.tool).copied().ok_or(
                    DocumentHydrationError::MissingBranchSnapshot {
                        feature: feature.id,
                        body: recipe.tool,
                    },
                )?;
                let tool =
                    snapshots
                        .get(&tool_id)
                        .ok_or(DocumentHydrationError::SnapshotUnavailable {
                            feature: feature.id,
                            snapshot: tool_id,
                        })?;
                let request = BooleanRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!("hydrate-boolean-{}", feature.id.get())),
                    expected_target_snapshot: input.id(),
                    expected_tool_snapshot: tool.id(),
                    precision: input.precision_policy().unwrap_or(options.root_precision),
                    operation: recipe.operation,
                };
                let outcome =
                    NativeKernel::execute_boolean(input, tool, &request, &CancellationToken::new())
                        .map_err(|error| DocumentHydrationError::Kernel {
                            feature: feature.id,
                            error,
                        })?;
                let association = association_from_report(&outcome.report);
                (association, Some(outcome.report), Some(outcome.snapshot))
            }
        };

        if let Some(snapshot) = output_snapshot {
            // Publish to the private staging archive before advancing a branch;
            // the next timeline feature can therefore replay immediately.
            snapshots.entry(snapshot.id()).or_insert(snapshot);
        }
        for body in &branches {
            branch_heads.insert(*body, association.output);
        }
        let provenance = verify_provenance(feature, association)?;
        features.push(HydratedFeature {
            feature: feature.id,
            branches,
            association,
            report,
            provenance,
        });
    }

    Ok(HydratedDocument {
        document,
        snapshots,
        features,
        branch_heads,
        skipped,
        beyond_history_cursor,
    })
}

fn execute_feature(
    feature: FeatureId,
    input: &Snapshot,
    command: artificer_protocol::KernelCommand,
    root_precision: PrecisionPolicy,
) -> Result<artificer_kernel::ExecutionOutcome, DocumentHydrationError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(format!("hydrate-feature-{}", feature.get())),
        expected_snapshot: input.id(),
        precision: input.precision_policy().unwrap_or(root_precision),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::new())
        .map_err(|error| DocumentHydrationError::Kernel { feature, error })
}

fn association_from_report(report: &OperationReport) -> SnapshotAssociation {
    SnapshotAssociation::new(
        report.input_snapshot,
        report.output_snapshot,
        report.semantic_digest,
    )
}

fn verify_provenance(
    feature: &FeatureNode,
    replayed: SnapshotAssociation,
) -> Result<HydratedProvenance, DocumentHydrationError> {
    if feature.state.rebuild == RebuildState::Dirty {
        return Ok(HydratedProvenance::Dirty {
            persisted: feature.committed,
        });
    }
    let persisted = feature
        .committed
        .ok_or(DocumentHydrationError::MissingCleanProvenance {
            feature: feature.id,
        })?;
    if persisted != replayed {
        return Err(DocumentHydrationError::ProvenanceMismatch {
            feature: feature.id,
            persisted: Box::new(persisted),
            replayed: Box::new(replayed),
        });
    }
    Ok(HydratedProvenance::Verified)
}

fn replay_input(
    document: &ModelDocument,
    feature: &FeatureNode,
    branches: &[BodyId],
    branch_heads: &BTreeMap<BodyId, SnapshotId>,
    snapshots: &BTreeMap<SnapshotId, Snapshot>,
    previous: Option<&HydratedFeature>,
) -> Result<SnapshotId, DocumentHydrationError> {
    let mut existing = Vec::new();
    let mut created = 0_usize;
    for body in branches {
        if document
            .body(*body)
            .is_some_and(|record| record.created_by == feature.id)
        {
            created += 1;
        } else {
            existing.push(*body);
        }
    }
    if created > 0 && !existing.is_empty() {
        return Err(DocumentHydrationError::MixedRootAndExistingBranches {
            feature: feature.id,
        });
    }
    if created > 0 {
        return Ok(SnapshotId::ZERO);
    }
    if !existing.is_empty() {
        let mut inputs = existing
            .iter()
            .map(|body| {
                branch_heads.get(body).copied().ok_or(
                    DocumentHydrationError::MissingBranchSnapshot {
                        feature: feature.id,
                        body: *body,
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        inputs.sort_unstable();
        inputs.dedup();
        if inputs.len() != 1 {
            return Err(DocumentHydrationError::DivergentBranchInputs {
                feature: feature.id,
                snapshots: inputs,
            });
        }
        return Ok(inputs[0]);
    }
    if !matches!(feature.action, ReplayAction::Marker) {
        return Err(DocumentHydrationError::KernelActionWithoutBody {
            feature: feature.id,
        });
    }

    if feature.state.rebuild == RebuildState::Clean {
        let persisted =
            feature
                .committed
                .ok_or(DocumentHydrationError::MissingCleanProvenance {
                    feature: feature.id,
                })?;
        if snapshots.contains_key(&persisted.input) {
            return Ok(persisted.input);
        }
        return Err(DocumentHydrationError::SnapshotUnavailable {
            feature: feature.id,
            snapshot: persisted.input,
        });
    }
    Ok(previous
        .map(|result| result.association.output)
        .unwrap_or(SnapshotId::ZERO))
}

fn feature_branches(document: &ModelDocument, feature: &FeatureNode) -> Vec<BodyId> {
    if let ReplayAction::Boolean(recipe) = &feature.action {
        return vec![recipe.target];
    }
    feature
        .inputs
        .iter()
        .filter_map(|input| match input {
            FeatureInput::Body(body) => Some(*body),
            FeatureInput::Sketch(sketch) => document
                .sketch(*sketch)
                .and_then(|record: &SketchRecord| record.support_body),
            FeatureInput::Feature(_) => None,
        })
        .chain(feature.outputs.iter().filter_map(|output| match output {
            FeatureOutput::Body(body) => Some(*body),
            FeatureOutput::Sketch { .. } => None,
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use artificer_model::persistent::{PersistentRef, TargetedKernel};
    use artificer_model::{
        ComponentContentDigest, ComponentDefinitionRef, ComponentDefinitionRevision,
        ComponentInstanceDraft, EvaluatedParameters, FeatureDraft, FeatureKind,
        KernelParameterBinding, KernelScalarTarget, OutputDraft, ParameterBinding, ParameterSpec,
        ParameterType, ParameterUnit, ParameterValue, ParameterizedKernel, QuantityKind,
        ReplayAction, RigidComponentPose, SketchId, SketchPayload, SketchRegionExtrusion,
        SketchRegionResolveError, SketchSupportRecipe,
    };
    use artificer_protocol::{
        EntityId, EntityKind, EntityRef, FaceExtrusionOperation, KernelCommand, OperationRole,
        PlanarFrame3, PlanarProfile2, Point2, Point3, SemanticDigest, Vector3,
    };
    use artificer_sketch::{
        Angle, ArrangementLimits, ConfirmationSource, Length, PointInput, RegionSignature,
        SignedLength, SketchDefinition, SketchPoint2, SketchRecipe, SketchValue, build_arrangement,
        compile_selected_profile,
    };

    use super::*;

    fn execute(
        input: &Snapshot,
        feature: u64,
        command: KernelCommand,
    ) -> artificer_kernel::ExecutionOutcome {
        NativeKernel::execute(
            input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new(format!("fixture-{feature}")),
                expected_snapshot: input.id(),
                precision: input.precision_policy().unwrap_or_default(),
                command,
            },
            &CancellationToken::new(),
        )
        .expect("fixture command should execute")
    }

    fn committed(report: &OperationReport) -> SnapshotAssociation {
        SnapshotAssociation::new(
            report.input_snapshot,
            report.output_snapshot,
            report.semantic_digest,
        )
    }

    fn cuboid(size_x: f64) -> KernelCommand {
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x,
            size_y: 3.0,
            size_z: 4.0,
        }
    }

    fn transform(dx: f64) -> KernelCommand {
        KernelCommand::TransformSnapshot {
            transform: artificer_protocol::SimilarityTransform3 {
                translation: Vector3::new(dx, 0.0, 0.0),
                ..artificer_protocol::SimilarityTransform3::identity()
            },
        }
    }

    #[test]
    fn hydration_resolves_parameterized_kernel_recipes_and_dirty_edits() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                ParameterSpec::new(
                    "length",
                    "Length",
                    ParameterType::Quantity(QuantityKind::Length),
                )
                .with_display_unit(ParameterUnit::Millimeter),
                ParameterBinding::literal(ParameterValue::quantity(7.0, ParameterUnit::Millimeter)),
            )
            .expect("length parameter should append");
        let recipe = ParameterizedKernel::independent(
            cuboid(1.0),
            vec![KernelParameterBinding::new(
                KernelScalarTarget::MakeCuboidSizeX,
                length,
            )],
        )
        .expect("parameterized cuboid recipe should validate");
        let expected = execute(&NativeKernel::empty(), 1, cuboid(7.0));
        let appended = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Parameterized body",
                    ReplayAction::ParameterizedKernel(recipe),
                )
                .with_parameter(length)
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".into(),
                })
                .with_commit(committed(&expected.report)),
            )
            .expect("parameterized body should append");
        let body = appended.created_bodies[0];

        let hydrated = hydrate_model_document(document.clone(), HydrationOptions::default())
            .expect("the saved recipe should bind and replay");
        assert_eq!(
            hydrated
                .branch_snapshot(body)
                .expect("body snapshot should be retained")
                .measures()
                .volume,
            84.0
        );
        assert_eq!(
            hydrated.features[0].provenance,
            HydratedProvenance::Verified
        );

        document
            .set_parameter_binding(
                length,
                ParameterBinding::literal(ParameterValue::quantity(8.0, ParameterUnit::Millimeter)),
            )
            .expect("parameter edit should mark the consuming feature dirty");
        let regenerated = hydrate_model_document(document, HydrationOptions::default())
            .expect("a dirty parameter edit should regenerate privately");
        assert_eq!(
            regenerated
                .branch_snapshot(body)
                .expect("regenerated body should be retained")
                .measures()
                .volume,
            96.0
        );
        assert!(matches!(
            regenerated.features[0].provenance,
            HydratedProvenance::Dirty { .. }
        ));
    }

    fn profile_extrusion(distance: f64) -> KernelCommand {
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2::from_polygon(&[
                Point2::new(0.0, 0.0),
                Point2::new(20.0, 0.0),
                Point2::new(20.0, 20.0),
                Point2::new(0.0, 20.0),
            ]),
            distance,
        }
    }

    fn editable_circle(radius: f64) -> (SketchDefinition, Vec<RegionSignature>, PlanarProfile2) {
        let mut definition = SketchDefinition::new();
        let transaction = definition
            .stage(
                SketchRecipe::CentrePointCircle {
                    center: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
                    radius: SketchValue::Literal(Length::new(radius).unwrap()),
                    radial_angle: SketchValue::Literal(Angle::radians(0.0).unwrap()),
                },
                "Circle",
            )
            .unwrap();
        definition
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(
            &definition.arrangement_inputs().unwrap(),
            &precision,
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 1);
        let regions = vec![arrangement.cells[0].signature.clone()];
        let profile = compile_selected_profile(&arrangement, &regions, &precision)
            .unwrap()
            .profile;
        (definition, regions, profile)
    }

    fn editable_rectangle(
        origin: SketchPoint2,
        width: f64,
        height: f64,
    ) -> (SketchDefinition, Vec<RegionSignature>, PlanarProfile2) {
        let mut definition = SketchDefinition::new();
        let transaction = definition
            .stage(
                SketchRecipe::TwoPointRectangle {
                    first_corner: PointInput::Position(origin),
                    width: SketchValue::Literal(SignedLength::new(width).unwrap()),
                    height: SketchValue::Literal(SignedLength::new(height).unwrap()),
                },
                "Rectangle",
            )
            .unwrap();
        definition
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(
            &definition.arrangement_inputs().unwrap(),
            &precision,
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 1);
        let regions = vec![arrangement.cells[0].signature.clone()];
        let profile = compile_selected_profile(&arrangement, &regions, &precision)
            .unwrap()
            .profile;
        (definition, regions, profile)
    }

    fn region_replay_document(
        radius: f64,
        distance: f64,
    ) -> (ModelDocument, SketchId, BodyId, FeatureId, RegionSignature) {
        let (definition, regions, profile) = editable_circle(radius);
        let frame = PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let empty = NativeKernel::empty();
        let mut document = ModelDocument::default();
        let sketch_feature = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker)
                    .with_sketch_payload(
                        SketchPayload::from_authoring(
                            frame,
                            definition,
                            Some(profile),
                            SketchSupportRecipe::Origin,
                        )
                        .unwrap(),
                    )
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 1".into(),
                        geometry_revision: 1,
                    })
                    .with_commit(SnapshotAssociation::new(
                        empty.id(),
                        empty.id(),
                        empty.semantic_digest(),
                    )),
            )
            .unwrap();
        let sketch = sketch_feature.created_sketches[0];
        let signature = regions[0].clone();
        let recipe = SketchRegionExtrusion::new_body(sketch, regions, distance).unwrap();
        let ReplayAction::Kernel(command) = recipe
            .resolve(&document, PrecisionPolicy::default())
            .unwrap()
        else {
            panic!("standalone region recipe should resolve independently")
        };
        let outcome = execute(&empty, 90, command);
        let extrusion = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Extrude,
                    "Extrude",
                    ReplayAction::SketchRegionExtrusion(recipe),
                )
                .with_input(FeatureInput::Sketch(sketch))
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".into(),
                })
                .with_commit(committed(&outcome.report)),
            )
            .unwrap();
        (
            document,
            sketch,
            extrusion.created_bodies[0],
            extrusion.feature,
            signature,
        )
    }

    #[test]
    fn save_load_and_upstream_edit_recompile_late_bound_exact_regions() {
        let (mut document, sketch, body, extrusion, signature) = region_replay_document(2.0, 5.0);
        let initial_json = serde_json::to_string(&document).unwrap();
        let initial = hydrate_document_json(&initial_json).unwrap();
        let initial_volume = initial.branch_snapshot(body).unwrap().measures().volume;
        assert!((initial_volume - std::f64::consts::PI * 4.0 * 5.0).abs() < 1.0e-8);
        assert!(matches!(
            initial.document.feature(extrusion).unwrap().action,
            ReplayAction::SketchRegionExtrusion(_)
        ));

        let (edited, edited_regions, edited_profile) = editable_circle(3.0);
        assert_eq!(edited_regions, vec![signature]);
        document
            .replace_sketch_payload(
                sketch,
                SketchPayload::from_authoring(
                    PlanarFrame3::new(
                        Point3::new(0.0, 0.0, 0.0),
                        Vector3::new(1.0, 0.0, 0.0),
                        Vector3::new(0.0, 1.0, 0.0),
                    ),
                    edited,
                    Some(edited_profile),
                    SketchSupportRecipe::Origin,
                )
                .unwrap(),
            )
            .unwrap();

        let edited_json = serde_json::to_string(&document).unwrap();
        let rebuilt = hydrate_document_json(&edited_json).unwrap();
        let edited_volume = rebuilt.branch_snapshot(body).unwrap().measures().volume;
        assert!((edited_volume - std::f64::consts::PI * 9.0 * 5.0).abs() < 1.0e-8);
        assert!(matches!(
            rebuilt
                .features
                .iter()
                .find(|result| result.feature == extrusion)
                .unwrap()
                .provenance,
            HydratedProvenance::Dirty { .. }
        ));
    }

    #[test]
    fn face_region_replay_late_binds_profile_target_operation_and_distance() {
        let empty = NativeKernel::empty();
        let base = execute(&empty, 1, cuboid(2.0));
        let mut document = ModelDocument::default();
        let base_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Base",
                    ReplayAction::Kernel(cuboid(2.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Body".into(),
                })
                .with_commit(committed(&base.report)),
            )
            .unwrap();
        let body = base_append.created_bodies[0];
        let face = PersistentRef::new(
            base_append.feature,
            OperationRole::new("face", Some(1)),
            EntityKind::Face,
        );
        let frame = PlanarFrame3::new(
            Point3::new(0.0, 0.0, 4.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let (definition, regions, profile) =
            editable_rectangle(SketchPoint2::new(0.5, 0.5), 1.0, 1.0);
        let sketch_append = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Face sketch", ReplayAction::Marker)
                    .with_input(FeatureInput::Body(body))
                    .with_sketch_payload(
                        SketchPayload::from_authoring(
                            frame,
                            definition,
                            Some(profile),
                            SketchSupportRecipe::PlanarFace {
                                body,
                                face: face.clone(),
                            },
                        )
                        .unwrap(),
                    )
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 1".into(),
                        geometry_revision: 1,
                    })
                    .with_commit(SnapshotAssociation::new(
                        base.snapshot.id(),
                        base.snapshot.id(),
                        base.snapshot.semantic_digest(),
                    )),
            )
            .unwrap();
        let sketch = sketch_append.created_sketches[0];
        let recipe =
            SketchRegionExtrusion::on_face(sketch, regions, face, FaceExtrusionOperation::Add, 1.0)
                .unwrap();
        let ReplayAction::TargetedKernel(targeted) = recipe
            .resolve(&document, PrecisionPolicy::default())
            .unwrap()
        else {
            panic!("face region recipe should retain a persistent target")
        };
        let command = match targeted.rebind(
            &[FeatureOperationReport::new(
                base_append.feature,
                &base.report,
            )],
            base.snapshot.id(),
        ) {
            PersistentResolution::Resolved(command) => command,
            other => panic!("face target should resolve: {other:?}"),
        };
        assert!(matches!(
            &command,
            KernelCommand::ExtrudeFacePlanarProfile {
                distance,
                operation: FaceExtrusionOperation::Add,
                ..
            } if *distance == 1.0
        ));
        let outcome = execute(&base.snapshot, 2, command);
        let feature = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Add,
                    "Boss",
                    ReplayAction::SketchRegionExtrusion(recipe),
                )
                .with_input(FeatureInput::Sketch(sketch))
                .with_input(FeatureInput::Body(body))
                .with_output(OutputDraft::ModifyBody(body))
                .with_commit(committed(&outcome.report)),
            )
            .unwrap()
            .feature;

        let hydrated = hydrate_document_json(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!((hydrated.branch_snapshot(body).unwrap().measures().volume - 25.0).abs() < 1.0e-9);
        assert!(hydrated.operation_report(feature).is_some());
    }

    #[test]
    fn unresolved_region_rejects_private_stage_and_retains_last_valid_runtime() {
        let (mut document, sketch, body, extrusion, selected) = region_replay_document(2.0, 5.0);
        let retained = hydrate_model_document(document.clone(), HydrationOptions::default())
            .expect("initial body should hydrate");
        let retained_snapshot = retained.branch_heads[&body];
        let retained_volume = retained.branch_snapshot(body).unwrap().measures().volume;

        // A fresh rectangle has different semantic boundary identities. The
        // replay feature must not guess by area, proximity, or cached profile.
        let mut rectangle = SketchDefinition::new();
        let transaction = rectangle
            .stage(
                SketchRecipe::TwoPointRectangle {
                    first_corner: PointInput::Position(SketchPoint2::new(-2.0, -2.0)),
                    width: SketchValue::Literal(SignedLength::new(4.0).unwrap()),
                    height: SketchValue::Literal(SignedLength::new(4.0).unwrap()),
                },
                "Rectangle",
            )
            .unwrap();
        rectangle
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(
            &rectangle.arrangement_inputs().unwrap(),
            &precision,
            ArrangementLimits::default(),
        );
        let rectangle_profile = compile_selected_profile(
            &arrangement,
            &[arrangement.cells[0].signature.clone()],
            &precision,
        )
        .unwrap()
        .profile;
        document
            .replace_sketch_payload(
                sketch,
                SketchPayload::from_authoring(
                    PlanarFrame3::new(
                        Point3::new(0.0, 0.0, 0.0),
                        Vector3::new(1.0, 0.0, 0.0),
                        Vector3::new(0.0, 1.0, 0.0),
                    ),
                    rectangle,
                    Some(rectangle_profile),
                    SketchSupportRecipe::Origin,
                )
                .unwrap(),
            )
            .unwrap();

        assert!(matches!(
            hydrate_model_document(document, HydrationOptions::default()),
            Err(DocumentHydrationError::SketchRegion {
                feature,
                error: SketchRegionResolveError::MissingRegion { signature, .. },
            }) if feature == extrusion && signature == selected
        ));
        assert_eq!(retained.branch_heads[&body], retained_snapshot);
        assert_eq!(
            retained.branch_snapshot(body).unwrap().measures().volume,
            retained_volume
        );
    }

    #[test]
    fn fresh_load_replays_independent_roots_and_a_chained_feature() {
        let empty = NativeKernel::empty();
        let first = execute(&empty, 1, cuboid(2.0));
        let second = execute(&empty, 2, cuboid(5.0));
        let moved = execute(&first.snapshot, 3, transform(10.0));
        let mut document = ModelDocument::default();
        let first_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "First",
                    ReplayAction::Kernel(cuboid(2.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".into(),
                })
                .with_commit(committed(&first.report)),
            )
            .unwrap();
        let second_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Second",
                    ReplayAction::Kernel(cuboid(5.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Body 2".into(),
                })
                .with_commit(committed(&second.report)),
            )
            .unwrap();
        let moved_feature = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Transform,
                    "Move first",
                    ReplayAction::Kernel(transform(10.0)),
                )
                .with_input(FeatureInput::Body(first_append.created_bodies[0]))
                .with_output(OutputDraft::ModifyBody(first_append.created_bodies[0]))
                .with_commit(committed(&moved.report)),
            )
            .unwrap()
            .feature;

        let json = serde_json::to_string(&document).unwrap();
        let hydrated = hydrate_document_json(&json).expect("all branches should hydrate");

        assert_eq!(hydrated.features.len(), 3);
        assert_eq!(
            hydrated.branch_heads[&first_append.created_bodies[0]],
            moved.snapshot.id()
        );
        assert_eq!(
            hydrated.branch_heads[&second_append.created_bodies[0]],
            second.snapshot.id()
        );
        assert_eq!(
            hydrated
                .branch_snapshot(first_append.created_bodies[0])
                .unwrap()
                .measures()
                .volume,
            24.0
        );
        assert_eq!(
            hydrated
                .operation_report(moved_feature)
                .unwrap()
                .input_snapshot,
            first.snapshot.id()
        );
        assert!(
            hydrated
                .features
                .iter()
                .all(|result| result.provenance == HydratedProvenance::Verified)
        );
    }

    #[test]
    fn component_root_is_reconstructed_and_component_suppression_is_honored() {
        let empty = NativeKernel::empty();
        let outcome = execute(&empty, 1, profile_extrusion(20.0));
        let definition = ComponentDefinitionRef::new(
            "aluminium-extrusion-20x20",
            ComponentDefinitionRevision::new(1, 0, 0),
            ComponentContentDigest::from_bytes([7; 32]),
        )
        .unwrap();
        let component = ComponentInstanceDraft::new(
            "Extrusion",
            definition,
            EvaluatedParameters::default(),
            RigidComponentPose::identity(),
        );
        let mut document = ModelDocument::default();
        let appended = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Insert component",
                    ReplayAction::Kernel(profile_extrusion(20.0)),
                )
                .with_component_instance(component)
                .with_output(OutputDraft::CreateBody {
                    label: "Component body".into(),
                })
                .with_commit(committed(&outcome.report)),
            )
            .unwrap();
        let component_id = appended.created_component_instance.unwrap();

        let hydrated = hydrate_document_json(&serde_json::to_string(&document).unwrap()).unwrap();
        let volume = hydrated
            .branch_snapshot(appended.created_bodies[0])
            .unwrap()
            .measures()
            .volume;
        assert!((volume - 8_000.0).abs() < 1.0e-9);

        document
            .set_component_suppressed(component_id, true)
            .unwrap();
        let suppressed = hydrate_document_json(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(
            !suppressed
                .branch_heads
                .contains_key(&appended.created_bodies[0])
        );
        assert_eq!(
            suppressed.skipped,
            vec![HydratedSkip {
                feature: appended.feature,
                reason: HydrationSkipReason::SuppressedComponent(component_id),
            }]
        );
    }

    #[test]
    fn targeted_feature_rebinds_only_from_reports_generated_during_load() {
        let empty = NativeKernel::empty();
        let base = execute(&empty, 1, cuboid(2.0));
        let mut document = ModelDocument::default();
        let base_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Base",
                    ReplayAction::Kernel(cuboid(2.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Body".into(),
                })
                .with_commit(committed(&base.report)),
            )
            .unwrap();
        let target = PersistentRef::new(
            base_append.feature,
            OperationRole::new("face", Some(1)),
            EntityKind::Face,
        );
        let template = KernelCommand::ExtrudeFaceProfile {
            target_face: EntityRef {
                snapshot: SnapshotId::new([0xaa; 16]),
                entity: EntityId(999),
                kind: EntityKind::Face,
            },
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 4.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            vertices: vec![
                Point2::new(0.5, 0.5),
                Point2::new(1.5, 0.5),
                Point2::new(1.5, 1.5),
                Point2::new(0.5, 1.5),
            ],
            distance: 1.0,
            operation: FaceExtrusionOperation::Add,
        };
        let targeted = TargetedKernel::new(template, target).unwrap();
        let bound = match targeted.clone().rebind(
            &[FeatureOperationReport::new(
                base_append.feature,
                &base.report,
            )],
            base.snapshot.id(),
        ) {
            PersistentResolution::Resolved(command) => command,
            other => panic!("fixture target should resolve: {other:?}"),
        };
        let edited = execute(&base.snapshot, 2, bound);
        let edited_feature = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Add,
                    "Boss",
                    ReplayAction::TargetedKernel(targeted),
                )
                .with_input(FeatureInput::Body(base_append.created_bodies[0]))
                .with_output(OutputDraft::ModifyBody(base_append.created_bodies[0]))
                .with_commit(committed(&edited.report)),
            )
            .unwrap()
            .feature;

        let hydrated = hydrate_document_json(&serde_json::to_string(&document).unwrap()).unwrap();
        assert_eq!(hydrated.features.len(), 2);
        assert_eq!(
            hydrated
                .operation_report(edited_feature)
                .unwrap()
                .semantic_digest,
            edited.report.semantic_digest
        );
    }

    #[test]
    fn suppression_and_history_cursor_bound_the_evaluated_runtime() {
        let empty = NativeKernel::empty();
        let base = execute(&empty, 1, cuboid(2.0));
        let moved = execute(&base.snapshot, 2, transform(2.0));
        let later = execute(&empty, 3, cuboid(9.0));
        let mut document = ModelDocument::default();
        let base_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Base",
                    ReplayAction::Kernel(cuboid(2.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Body".into(),
                })
                .with_commit(committed(&base.report)),
            )
            .unwrap();
        let move_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Transform,
                    "Move",
                    ReplayAction::Kernel(transform(2.0)),
                )
                .with_input(FeatureInput::Body(base_append.created_bodies[0]))
                .with_output(OutputDraft::ModifyBody(base_append.created_bodies[0]))
                .with_commit(committed(&moved.report)),
            )
            .unwrap();
        let later_append = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Later",
                    ReplayAction::Kernel(cuboid(9.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Later body".into(),
                })
                .with_commit(committed(&later.report)),
            )
            .unwrap();
        document
            .set_feature_suppressed(move_append.feature, true)
            .unwrap();
        document.set_history_position(2).unwrap();

        let hydrated = hydrate_document_json(&serde_json::to_string(&document).unwrap()).unwrap();
        assert_eq!(hydrated.features.len(), 1);
        assert_eq!(
            hydrated.branch_heads[&base_append.created_bodies[0]],
            base.snapshot.id()
        );
        assert_eq!(
            hydrated.skipped[0].reason,
            HydrationSkipReason::ExplicitSuppression
        );
        assert_eq!(hydrated.beyond_history_cursor, vec![later_append.feature]);
        assert!(
            !hydrated
                .branch_heads
                .contains_key(&later_append.created_bodies[0])
        );
    }

    #[test]
    fn clean_provenance_tampering_rejects_the_whole_stage() {
        let empty = NativeKernel::empty();
        let base = execute(&empty, 1, cuboid(2.0));
        let mut document = ModelDocument::default();
        let appended = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Base",
                    ReplayAction::Kernel(cuboid(2.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Body".into(),
                })
                .with_commit(committed(&base.report)),
            )
            .unwrap();
        let mut value = serde_json::to_value(document).unwrap();
        value["state"]["features"][0]["committed"]["semantic_digest"] =
            serde_json::Value::String(SemanticDigest::new([0x55; 32]).to_string());

        assert!(matches!(
            hydrate_document_json(&serde_json::to_string(&value).unwrap()),
            Err(DocumentHydrationError::ProvenanceMismatch { feature, .. })
                if feature == appended.feature
        ));
    }

    #[test]
    fn late_kernel_failure_returns_no_partial_runtime() {
        let empty = NativeKernel::empty();
        let first = execute(&empty, 1, cuboid(2.0));
        let second = execute(&empty, 2, cuboid(3.0));
        let mut document = ModelDocument::default();
        document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "First",
                    ReplayAction::Kernel(cuboid(2.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "First body".into(),
                })
                .with_commit(committed(&first.report)),
            )
            .unwrap();
        let second_feature = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Second",
                    ReplayAction::Kernel(cuboid(3.0)),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Second body".into(),
                })
                .with_commit(committed(&second.report)),
            )
            .unwrap()
            .feature;
        document
            .replace_feature_action(second_feature, ReplayAction::Kernel(cuboid(-3.0)))
            .unwrap();

        let result = hydrate_document_json(&serde_json::to_string(&document).unwrap());
        assert!(matches!(
            result,
            Err(DocumentHydrationError::Kernel { feature, .. }) if feature == second_feature
        ));
    }
}

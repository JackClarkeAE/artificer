//! Versioned, deterministic document history for Artificer.
//!
//! This crate owns document identity and replay intent. It deliberately does
//! not execute kernel commands, retain renderer state, or depend on a UI. A
//! caller asks for a deterministic [`RebuildPlan`], executes its steps through
//! the public kernel facade, and atomically commits the resulting snapshot
//! associations. Dropping or explicitly rolling back a transaction cannot
//! mutate the last committed document state.

pub mod assembly;
pub mod components;
pub mod parameterized;
pub mod parameters;
pub mod persistent;
pub mod sketch_region;
pub mod sketches;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use artificer_protocol::{BooleanOperation, KernelCommand, SemanticDigest, SnapshotId};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

pub use assembly::{
    JointAxis, JointDraft, JointError, JointKind, JointOrigin, JointParent, JointRecord,
    RevoluteLimits,
};
pub use components::{
    CanonicalQuaternion, ComponentContentDigest, ComponentDefinitionRef,
    ComponentDefinitionRevision, ComponentError, ComponentInstanceDraft, ComponentInstanceRecord,
    ComponentTranslation, RigidComponentPose,
};
pub use parameterized::{
    KernelParameterBinding, KernelReplayTemplate, KernelScalarTarget,
    MAX_KERNEL_PARAMETER_BINDINGS, ParameterizedKernel, ParameterizedKernelError,
};
pub use parameters::{
    EvaluatedParameters, ParameterBinding, ParameterBindingDigest, ParameterChoice, ParameterError,
    ParameterExposure, ParameterExpression, ParameterMetadata, ParameterOverrides, ParameterRecord,
    ParameterSpec, ParameterTable, ParameterType, ParameterUnit, ParameterValue, QuantityKind,
    QuantityValue,
};
pub use sketch_region::{
    CURRENT_SKETCH_REGION_RECIPE_VERSION, MAX_SELECTED_SKETCH_REGIONS, SketchRegionExtrusion,
    SketchRegionExtrusionTarget, SketchRegionRecipeError, SketchRegionResolveError,
};
pub use sketches::{
    CURRENT_SKETCH_PRECISION_POLICY_VERSION, SketchPayload, SketchPayloadError, SketchSupportRecipe,
};

/// Stable native document format marker.
pub const NATIVE_DOCUMENT_FORMAT: &str = "artificer.native.document";
/// Native document schema written by this version of Artificer.
///
/// Version 4 introduced portable sketch payloads and typed parameterized
/// kernel recipes. Version 5 adds the persistent assembly joint forest.
/// Version 6 adds authoritative editable sketch-operation graphs.
pub const CURRENT_DOCUMENT_VERSION: u32 = 6;
/// First native schema that requires exact portable sketch payloads.
pub const PORTABLE_SKETCH_DOCUMENT_VERSION: u32 = 4;
/// First native schema with a persistent assembly hierarchy and joint graph.
pub const ASSEMBLY_JOINT_DOCUMENT_VERSION: u32 = 5;
/// First native schema that requires an authoritative editable sketch graph.
pub const EDITABLE_SKETCH_DOCUMENT_VERSION: u32 = 6;
/// Oldest native document schema this version can migrate in memory.
pub const MIN_SUPPORTED_DOCUMENT_VERSION: u32 = 1;
/// Hard ceiling for one document's ordered feature timeline.
pub const MAX_FEATURES: usize = 4_096;
/// Hard ceiling for bodies and sketches, independently.
pub const MAX_OBJECTS_PER_KIND: usize = 4_096;
/// Hard ceiling for inputs, dependencies, or outputs on one feature.
pub const MAX_NODE_REFERENCES: usize = 64;
/// Hard ceiling for user-visible names stored in the document.
pub const MAX_LABEL_BYTES: usize = 128;
/// Default number of user edits retained by the runtime undo journal.
pub const DEFAULT_UNDO_LIMIT: usize = 128;
/// Hard ceiling for the runtime undo preference.
pub const MAX_UNDO_LIMIT: usize = 1_024;

macro_rules! stable_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Stable document-level `", stringify!($name), "`.")]
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Returns the non-zero serialized integer carried by this ID.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }

            const fn from_allocated(value: u64) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

stable_id!(FeatureId, "feature:");
stable_id!(BodyId, "body:");
stable_id!(SketchId, "sketch:");
stable_id!(ParameterId, "parameter:");
stable_id!(ComponentInstanceId, "component:");
stable_id!(JointId, "joint:");

/// Broad feature class used by the Browser and timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureKind {
    Origin,
    DatumPlane,
    BaseBody,
    Sketch,
    Extrude,
    Add,
    Cut,
    Transform,
    Boolean,
}

/// Stable two-body Boolean intent. The target owns the successor snapshot;
/// the tool remains a distinct document body and may be retained visibly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BooleanFeatureRecipe {
    pub target: BodyId,
    pub tool: BodyId,
    pub operation: BooleanOperation,
    #[serde(default)]
    pub keep_tool: bool,
}

/// Stable input to a feature recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum FeatureInput {
    Feature(FeatureId),
    Body(BodyId),
    Sketch(SketchId),
}

/// Stable output touched by a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum FeatureOutput {
    Body(BodyId),
    Sketch {
        sketch: SketchId,
        geometry_revision: u64,
    },
}

impl FeatureOutput {
    #[must_use]
    pub const fn object(self) -> FeatureInput {
        match self {
            Self::Body(body) => FeatureInput::Body(body),
            Self::Sketch { sketch, .. } => FeatureInput::Sketch(sketch),
        }
    }
}

/// Serializable replay intent.
///
/// `Marker` is used by document-only nodes such as a committed sketch. Kernel
/// commands are retained without an `ExecuteRequest`: the rebuild coordinator
/// owns request IDs, precision policy, cancellation, and the current snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "command", rename_all = "snake_case")]
pub enum ReplayAction {
    Marker,
    /// Snapshot-independent command with a document-owned late-bound target.
    TargetedKernel(persistent::TargetedKernel),
    /// A kernel command with no persistent entity target.
    ///
    /// `ExtrudeFaceProfile` is rejected here because its raw `EntityRef` is
    /// snapshot-scoped. Store it in [`ReplayAction::TargetedKernel`] instead.
    Kernel(KernelCommand),
    /// Kernel-command template whose typed scalar fields are resolved from the
    /// feature's stable parameter inputs immediately before replay.
    ParameterizedKernel(ParameterizedKernel),
    /// Exact profile feature whose region set is resolved from the current
    /// editable sketch graph immediately before replay.
    SketchRegionExtrusion(SketchRegionExtrusion),
    /// Two-snapshot native Boolean resolved by the replay coordinator.
    Boolean(BooleanFeatureRecipe),
}

impl ReplayAction {
    /// Resolves typed scalar bindings into an ordinary replay action.
    ///
    /// Marker and already-concrete actions are cloned unchanged. A successful
    /// parameterized resolution always returns either [`Self::Kernel`] or
    /// [`Self::TargetedKernel`], never another parameterized action.
    pub fn resolve_parameters(
        &self,
        parameters: &EvaluatedParameters,
    ) -> Result<Self, ParameterizedKernelError> {
        match self {
            Self::ParameterizedKernel(recipe) => recipe.resolve(parameters),
            Self::Marker
            | Self::TargetedKernel(_)
            | Self::Kernel(_)
            | Self::SketchRegionExtrusion(_)
            | Self::Boolean(_) => Ok(self.clone()),
        }
    }

    /// Resolves a late-bound region selection into the existing independent
    /// or persistent-face kernel replay path.
    pub fn resolve_sketch_regions(
        &self,
        document: &ModelDocument,
        precision: artificer_protocol::PrecisionPolicy,
    ) -> Result<Self, SketchRegionResolveError> {
        match self {
            Self::SketchRegionExtrusion(recipe) => recipe.resolve(document, precision),
            Self::Marker
            | Self::TargetedKernel(_)
            | Self::Kernel(_)
            | Self::ParameterizedKernel(_)
            | Self::Boolean(_) => Ok(self.clone()),
        }
    }
}

/// Last successful immutable-kernel association for one feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAssociation {
    pub input: SnapshotId,
    pub output: SnapshotId,
    pub semantic_digest: SemanticDigest,
}

impl SnapshotAssociation {
    #[must_use]
    pub const fn new(
        input: SnapshotId,
        output: SnapshotId,
        semantic_digest: SemanticDigest,
    ) -> Self {
        Self {
            input,
            output,
            semantic_digest,
        }
    }
}

/// Whether a feature's retained result matches its current recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuildState {
    Clean,
    Dirty,
}

/// Editable state kept with an ordered feature node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureState {
    pub suppressed: bool,
    pub read_only: bool,
    pub rebuild: RebuildState,
}

/// One ordered, stable node in the parametric document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureNode {
    pub id: FeatureId,
    pub kind: FeatureKind,
    pub label: String,
    pub inputs: Vec<FeatureInput>,
    /// Typed document parameters consumed by this replay recipe.
    #[serde(default)]
    pub parameter_inputs: Vec<ParameterId>,
    /// Component occurrence atomically created by this feature, when present.
    #[serde(default)]
    pub component_instance: Option<ComponentInstanceId>,
    /// Exact, revision-specific sketch geometry and stable support recipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sketch_payload: Option<SketchPayload>,
    pub dependencies: Vec<FeatureId>,
    pub outputs: Vec<FeatureOutput>,
    pub action: ReplayAction,
    pub state: FeatureState,
    pub committed: Option<SnapshotAssociation>,
}

/// Persistent Browser record for one logical body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyRecord {
    pub id: BodyId,
    pub label: String,
    pub created_by: FeatureId,
    pub last_feature: FeatureId,
    pub visible: bool,
    pub read_only: bool,
    pub committed_snapshot: Option<SnapshotId>,
}

/// Persistent Browser record for one logical sketch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchRecord {
    pub id: SketchId,
    pub label: String,
    pub created_by: FeatureId,
    pub last_feature: FeatureId,
    /// Body whose support established this sketch, or `None` for an origin-plane sketch.
    pub support_body: Option<BodyId>,
    pub geometry_revision: u64,
    pub visible: bool,
    /// Modeling feature that supplied the default auto-hide, distinct from an
    /// explicit user visibility choice.
    #[serde(default)]
    pub auto_hidden_by: Option<FeatureId>,
    pub read_only: bool,
    pub committed_snapshot: Option<SnapshotId>,
}

/// Output requested by a new feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputDraft {
    CreateBody {
        label: String,
    },
    ModifyBody(BodyId),
    CreateSketch {
        label: String,
        geometry_revision: u64,
    },
    ModifySketch {
        sketch: SketchId,
        geometry_revision: u64,
    },
}

/// Validated input for appending one feature atomically.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureDraft {
    pub kind: FeatureKind,
    pub label: String,
    pub inputs: Vec<FeatureInput>,
    pub parameter_inputs: Vec<ParameterId>,
    pub component_instance: Option<ComponentInstanceDraft>,
    pub sketch_payload: Option<SketchPayload>,
    pub dependencies: Vec<FeatureId>,
    pub outputs: Vec<OutputDraft>,
    pub action: ReplayAction,
    pub committed: Option<SnapshotAssociation>,
    pub read_only: bool,
}

impl FeatureDraft {
    #[must_use]
    pub fn new(kind: FeatureKind, label: impl Into<String>, action: ReplayAction) -> Self {
        Self {
            kind,
            label: label.into(),
            inputs: Vec::new(),
            parameter_inputs: Vec::new(),
            component_instance: None,
            sketch_payload: None,
            dependencies: Vec::new(),
            outputs: Vec::new(),
            action,
            committed: None,
            read_only: false,
        }
    }

    #[must_use]
    pub fn with_input(mut self, input: FeatureInput) -> Self {
        self.inputs.push(input);
        self
    }

    /// Declares a typed parameter consumed when late-binding this recipe.
    #[must_use]
    pub fn with_parameter(mut self, parameter: ParameterId) -> Self {
        self.parameter_inputs.push(parameter);
        self
    }

    /// Stages an immutable component occurrence whose bodies are the newly
    /// created body outputs of this same feature transaction.
    #[must_use]
    pub fn with_component_instance(mut self, component: ComponentInstanceDraft) -> Self {
        self.component_instance = Some(component);
        self
    }

    /// Attaches the exact portable geometry produced by this sketch revision.
    #[must_use]
    pub fn with_sketch_payload(mut self, payload: SketchPayload) -> Self {
        self.sketch_payload = Some(payload);
        self
    }

    #[must_use]
    pub fn with_dependency(mut self, dependency: FeatureId) -> Self {
        self.dependencies.push(dependency);
        self
    }

    #[must_use]
    pub fn with_output(mut self, output: OutputDraft) -> Self {
        self.outputs.push(output);
        self
    }

    #[must_use]
    pub const fn with_commit(mut self, committed: SnapshotAssociation) -> Self {
        self.committed = Some(committed);
        self
    }

    #[must_use]
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }
}

/// IDs allocated while appending a feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendFeatureResult {
    pub feature: FeatureId,
    pub created_bodies: Vec<BodyId>,
    pub created_sketches: Vec<SketchId>,
    pub created_component_instance: Option<ComponentInstanceId>,
}

/// Persisted evaluation boundary for the ordered feature timeline.
///
/// This is deliberately independent of [`FeatureState::suppressed`]. A
/// suppressed feature remains inside the evaluated prefix and is omitted by
/// replay semantics; a feature after this cursor is rolled back wholesale and
/// retains its recipe, suppression flag, and cached association for a later
/// roll-forward.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "position", content = "feature", rename_all = "snake_case")]
pub enum HistoryCursor {
    /// Evaluate no feature nodes.
    Start,
    /// Evaluate through and including this stable feature.
    After(FeatureId),
    /// Evaluate the entire timeline and automatically include later appends.
    #[default]
    End,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct DocumentState {
    features: Vec<FeatureNode>,
    bodies: Vec<BodyRecord>,
    sketches: Vec<SketchRecord>,
    /// Typed part parameters. Added in native document v3.
    #[serde(default)]
    parameters: ParameterTable,
    /// Rigid component occurrences. Added additively to native document v3.
    #[serde(default)]
    component_instances: Vec<ComponentInstanceRecord>,
    /// Directed rigid-joint forest. Added in native document v5.
    #[serde(default)]
    joints: Vec<JointRecord>,
    head_snapshot: Option<SnapshotId>,
    #[serde(default)]
    history_cursor: HistoryCursor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AllocatorState {
    next_feature: u64,
    next_body: u64,
    next_sketch: u64,
    #[serde(default = "default_next_stable_id")]
    next_parameter: u64,
    #[serde(default = "default_next_stable_id")]
    next_component_instance: u64,
    #[serde(default = "default_next_stable_id")]
    next_joint: u64,
}

const fn default_next_stable_id() -> u64 {
    1
}

impl Default for AllocatorState {
    fn default() -> Self {
        Self {
            next_feature: 1,
            next_body: 1,
            next_sketch: 1,
            next_parameter: 1,
            next_component_instance: 1,
            next_joint: 1,
        }
    }
}

/// Versioned, serde-ready native document envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeDocument {
    format: String,
    version: u32,
    revision: u64,
    undo_limit: usize,
    allocators: AllocatorState,
    state: DocumentState,
    /// Sketch features migrated from v1-v3 whose exact geometry was never
    /// present in those schemas. New v4 authoring cannot add to this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    legacy_sketch_payload_omissions: Vec<FeatureId>,
}

impl NativeDocument {
    #[must_use]
    pub fn format(&self) -> &str {
        &self.format
    }

    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// Runtime document plus a bounded, non-serialized undo journal.
#[derive(Clone, Debug)]
pub struct ModelDocument {
    state: DocumentState,
    allocators: AllocatorState,
    revision: u64,
    undo_limit: usize,
    undo: VecDeque<DocumentState>,
    redo: VecDeque<DocumentState>,
    legacy_sketch_payload_omissions: BTreeSet<FeatureId>,
}

impl Default for ModelDocument {
    fn default() -> Self {
        Self {
            state: DocumentState::default(),
            allocators: AllocatorState::default(),
            revision: 0,
            undo_limit: DEFAULT_UNDO_LIMIT,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            legacy_sketch_payload_omissions: BTreeSet::new(),
        }
    }
}

impl Serialize for ModelDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_native().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ModelDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let native = NativeDocument::deserialize(deserializer)?;
        Self::from_native(native).map_err(de::Error::custom)
    }
}

impl ModelDocument {
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn features(&self) -> &[FeatureNode] {
        &self.state.features
    }

    /// Persisted feature-history evaluation boundary.
    #[must_use]
    pub const fn history_cursor(&self) -> HistoryCursor {
        self.state.history_cursor
    }

    /// Number of feature nodes currently inside the evaluated history prefix.
    #[must_use]
    pub fn history_position(&self) -> usize {
        active_feature_count(&self.state)
            .expect("a runtime document always carries a validated history cursor")
    }

    /// Total number of stable positions exposed to a zero-based history slider.
    ///
    /// Position zero is before the first feature and `features().len()` is the
    /// fully rolled-forward state.
    #[must_use]
    pub fn history_position_count(&self) -> usize {
        self.state.features.len() + 1
    }

    /// Feature nodes inside the currently evaluated history prefix.
    #[must_use]
    pub fn active_features(&self) -> &[FeatureNode] {
        &self.state.features[..self.history_position()]
    }

    /// Reports whether one retained feature is inside the evaluation prefix.
    pub fn feature_is_active(&self, id: FeatureId) -> Result<bool, DocumentError> {
        Ok(self.feature_index(id)? < self.history_position())
    }

    /// Moves the history boundary to a slider position in `0..=features.len()`.
    ///
    /// One successful move creates one bounded undo checkpoint. Callers should
    /// commit a drag only when its pointer interaction ends rather than calling
    /// this for every rendered frame.
    pub fn set_history_position(&mut self, position: usize) -> Result<bool, DocumentError> {
        if position > self.state.features.len() {
            return Err(DocumentError::HistoryPositionOutOfRange {
                position,
                feature_count: self.state.features.len(),
            });
        }
        let cursor = if position == self.state.features.len() {
            HistoryCursor::End
        } else if position == 0 {
            HistoryCursor::Start
        } else {
            HistoryCursor::After(self.state.features[position - 1].id)
        };
        self.set_history_cursor(cursor)
    }

    /// Moves to an explicit stable feature boundary.
    pub fn set_history_cursor(&mut self, cursor: HistoryCursor) -> Result<bool, DocumentError> {
        let position = history_cursor_position(&self.state.features, cursor)?;
        let current_position = self.history_position();
        let canonical = if position == self.state.features.len() {
            HistoryCursor::End
        } else if position == 0 {
            HistoryCursor::Start
        } else {
            HistoryCursor::After(self.state.features[position - 1].id)
        };
        if current_position == position && self.state.history_cursor == canonical {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.history_cursor = canonical;
        reconcile_active_object_state(&mut self.state);
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Rolls the history boundary backward by one retained feature.
    pub fn step_history_backward(&mut self) -> Result<bool, DocumentError> {
        let position = self.history_position();
        if position == 0 {
            return Ok(false);
        }
        self.set_history_position(position - 1)
    }

    /// Rolls the history boundary forward by one retained feature.
    pub fn step_history_forward(&mut self) -> Result<bool, DocumentError> {
        let position = self.history_position();
        if position == self.state.features.len() {
            return Ok(false);
        }
        self.set_history_position(position + 1)
    }

    #[must_use]
    pub fn bodies(&self) -> &[BodyRecord] {
        &self.state.bodies
    }

    #[must_use]
    pub fn sketches(&self) -> &[SketchRecord] {
        &self.state.sketches
    }

    /// Typed parameter table persisted with this part document.
    #[must_use]
    pub const fn parameters(&self) -> &ParameterTable {
        &self.state.parameters
    }

    #[must_use]
    pub fn parameter(&self, id: ParameterId) -> Option<&ParameterRecord> {
        self.state.parameters.get(id)
    }

    /// Allocates and appends one stable typed parameter atomically.
    pub fn add_parameter(
        &mut self,
        spec: ParameterSpec,
        binding: ParameterBinding,
    ) -> Result<ParameterId, DocumentError> {
        let id = ParameterId::from_allocated(self.allocators.next_parameter);
        let next = checked_next(self.allocators.next_parameter, "parameter IDs")?;
        let previous = self.state.clone();
        self.state.parameters.insert_allocated(id, spec, binding)?;
        self.allocators.next_parameter = next;
        self.finish_user_edit(previous);
        Ok(id)
    }

    /// Replaces parameter metadata/type information and dirties every feature
    /// consuming the parameter or one of its derived dependents.
    pub fn replace_parameter_spec(
        &mut self,
        id: ParameterId,
        spec: ParameterSpec,
    ) -> Result<bool, DocumentError> {
        let previous = self.state.clone();
        let mut affected = self.state.parameters.affected_by(id);
        if !self.state.parameters.replace_spec(id, spec)? {
            return Ok(false);
        }
        if let Err(error) = validate_all_action_parameter_inputs(&self.state) {
            self.state = previous;
            return Err(error);
        }
        affected.extend(self.state.parameters.affected_by(id));
        self.mark_parameter_consumers_dirty(&affected);
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Replaces a literal/expression/unresolved binding atomically.
    pub fn set_parameter_binding(
        &mut self,
        id: ParameterId,
        binding: ParameterBinding,
    ) -> Result<bool, DocumentError> {
        let previous = self.state.clone();
        let mut affected = self.state.parameters.affected_by(id);
        if !self.state.parameters.set_binding(id, binding)? {
            return Ok(false);
        }
        affected.extend(self.state.parameters.affected_by(id));
        self.mark_parameter_consumers_dirty(&affected);
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Removes an unused parameter. Feature consumers and derived parameter
    /// references must be removed first; rejection leaves the document intact.
    pub fn remove_parameter(&mut self, id: ParameterId) -> Result<ParameterRecord, DocumentError> {
        if let Some(feature) = self
            .state
            .features
            .iter()
            .find(|feature| feature.parameter_inputs.contains(&id))
        {
            return Err(DocumentError::ParameterInUse {
                parameter: id,
                feature: feature.id,
            });
        }
        let previous = self.state.clone();
        let removed = self.state.parameters.remove(id)?;
        self.finish_user_edit(previous);
        Ok(removed)
    }

    /// Resolves the canonical parameter assignment used to late-bind kernel
    /// commands and key resolved variant caches.
    pub fn evaluate_parameters(
        &self,
        overrides: &ParameterOverrides,
    ) -> Result<EvaluatedParameters, ParameterError> {
        self.state.parameters.evaluate(overrides)
    }

    /// Stable rigid component occurrences in Browser order.
    #[must_use]
    pub fn component_instances(&self) -> &[ComponentInstanceRecord] {
        &self.state.component_instances
    }

    #[must_use]
    pub fn component_instance(&self, id: ComponentInstanceId) -> Option<&ComponentInstanceRecord> {
        self.state
            .component_instances
            .iter()
            .find(|component| component.id == id)
    }

    /// Updates only the rigid occurrence transform. No kernel feature becomes
    /// dirty because occurrence placement is assembly state, not body geometry.
    pub fn set_component_pose(
        &mut self,
        id: ComponentInstanceId,
        pose: RigidComponentPose,
    ) -> Result<bool, DocumentError> {
        pose.validate()?;
        let index = self.component_instance_index(id)?;
        if self.state.component_instances[index].grounded {
            return Err(DocumentError::GroundedComponent(id));
        }
        if self.state.component_instances[index].pose == pose {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.component_instances[index].pose = pose;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_component_visible(
        &mut self,
        id: ComponentInstanceId,
        visible: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.component_instance_index(id)?;
        if self.state.component_instances[index].visible == visible {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.component_instances[index].visible = visible;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_component_suppressed(
        &mut self,
        id: ComponentInstanceId,
        suppressed: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.component_instance_index(id)?;
        if self.state.component_instances[index].suppressed == suppressed {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.component_instances[index].suppressed = suppressed;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_component_grounded(
        &mut self,
        id: ComponentInstanceId,
        grounded: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.component_instance_index(id)?;
        if self.state.component_instances[index].grounded == grounded {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.component_instances[index].grounded = grounded;
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Stable assembly joints in Browser order.
    #[must_use]
    pub fn joints(&self) -> &[JointRecord] {
        &self.state.joints
    }

    #[must_use]
    pub fn joint(&self, id: JointId) -> Option<&JointRecord> {
        self.state.joints.iter().find(|joint| joint.id == id)
    }

    /// Returns the sole retained parent joint for a component, when present.
    #[must_use]
    pub fn joint_for_child(&self, child: ComponentInstanceId) -> Option<&JointRecord> {
        self.state.joints.iter().find(|joint| joint.child == child)
    }

    /// Adds one validated edge to the bounded assembly forest.
    ///
    /// Disabled joints remain structural edges: disabling motion does not
    /// silently reparent a component or make a cyclic archive valid.
    pub fn add_joint(&mut self, draft: JointDraft) -> Result<JointId, DocumentError> {
        if self.state.joints.len() == assembly::MAX_JOINTS {
            return Err(DocumentError::CapacityExceeded {
                resource: "assembly joints",
                limit: assembly::MAX_JOINTS,
            });
        }
        draft.validate()?;
        self.validate_joint_edge(draft.parent, draft.child, None)?;
        let id = JointId::from_allocated(self.allocators.next_joint);
        let next = checked_next(self.allocators.next_joint, "joint IDs")?;
        let record = JointRecord::from_draft(id, draft)?;
        let previous = self.state.clone();
        self.state.joints.push(record);
        self.allocators.next_joint = next;
        self.finish_user_edit(previous);
        Ok(id)
    }

    /// Removes one hierarchy edge without removing either component endpoint.
    pub fn remove_joint(&mut self, id: JointId) -> Result<JointRecord, DocumentError> {
        let index = self.joint_index(id)?;
        let previous = self.state.clone();
        let removed = self.state.joints.remove(index);
        self.finish_user_edit(previous);
        Ok(removed)
    }

    pub fn rename_joint(
        &mut self,
        id: JointId,
        name: impl Into<String>,
    ) -> Result<bool, DocumentError> {
        let name = name.into();
        assembly::validate_joint_name(&name)?;
        let index = self.joint_index(id)?;
        if self.state.joints[index].name == name {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.joints[index].name = name;
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Reparents a retained joint atomically after checking the entire graph.
    pub fn set_joint_parent(
        &mut self,
        id: JointId,
        parent: JointParent,
    ) -> Result<bool, DocumentError> {
        let index = self.joint_index(id)?;
        if self.state.joints[index].parent == parent {
            return Ok(false);
        }
        let child = self.state.joints[index].child;
        self.validate_joint_edge(parent, child, Some(id))?;
        let previous = self.state.clone();
        self.state.joints[index].parent = parent;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_joint_kind(&mut self, id: JointId, kind: JointKind) -> Result<bool, DocumentError> {
        kind.validate()?;
        let index = self.joint_index(id)?;
        if self.state.joints[index].kind == kind {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.joints[index].kind = kind;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_joint_enabled(&mut self, id: JointId, enabled: bool) -> Result<bool, DocumentError> {
        let index = self.joint_index(id)?;
        if self.state.joints[index].enabled == enabled {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.joints[index].enabled = enabled;
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Returns the most recently published snapshot on any body branch.
    ///
    /// This is an activity cursor, not a combined-document snapshot. Use each
    /// [`BodyRecord::committed_snapshot`] when executing a body-local command.
    #[must_use]
    pub const fn head_snapshot(&self) -> Option<SnapshotId> {
        self.state.head_snapshot
    }

    #[must_use]
    pub fn feature(&self, id: FeatureId) -> Option<&FeatureNode> {
        self.state.features.iter().find(|feature| feature.id == id)
    }

    #[must_use]
    pub fn body(&self, id: BodyId) -> Option<&BodyRecord> {
        self.state.bodies.iter().find(|body| body.id == id)
    }

    #[must_use]
    pub fn sketch(&self, id: SketchId) -> Option<&SketchRecord> {
        self.state.sketches.iter().find(|sketch| sketch.id == id)
    }

    /// Returns the exact payload authored for one retained sketch revision.
    ///
    /// Legacy v1-v3 revisions can legitimately return `None` because those
    /// schemas never persisted sketch geometry. A v4-authored revision always
    /// has a payload.
    #[must_use]
    pub fn sketch_payload(
        &self,
        sketch: SketchId,
        geometry_revision: u64,
    ) -> Option<&SketchPayload> {
        self.state.features.iter().rev().find_map(|feature| {
            feature
                .outputs
                .iter()
                .any(|output| {
                    *output
                        == (FeatureOutput::Sketch {
                            sketch,
                            geometry_revision,
                        })
                })
                .then_some(feature.sketch_payload.as_ref())
                .flatten()
        })
    }

    /// Replaces the current authored revision in place and dirties every
    /// dependent feature. Undo retains the complete previous payload and
    /// geometry revision, while downstream region recipes keep the same
    /// logical [`SketchId`].
    pub fn replace_sketch_payload(
        &mut self,
        sketch: SketchId,
        payload: SketchPayload,
    ) -> Result<bool, DocumentError> {
        payload.validate()?;
        let sketch_index = self.sketch_index(sketch)?;
        let record = &self.state.sketches[sketch_index];
        if record.read_only {
            return Err(DocumentError::ReadOnlySketch(sketch));
        }
        let feature_index = self.feature_index(record.last_feature)?;
        let feature = &self.state.features[feature_index];
        if feature.state.read_only {
            return Err(DocumentError::ReadOnlyFeature(feature.id));
        }
        if feature.kind != FeatureKind::Sketch
            || feature.outputs.as_slice()
                != [FeatureOutput::Sketch {
                    sketch,
                    geometry_revision: record.geometry_revision,
                }]
        {
            return Err(DocumentError::InvalidSketchPayloadOutput);
        }
        match &payload.support {
            SketchSupportRecipe::Origin if record.support_body.is_none() => {}
            SketchSupportRecipe::PlanarFace { body, face }
                if record.support_body == Some(*body) && self.body(*body).is_some() =>
            {
                for producer in persistent_ref_producers(face) {
                    if self.feature(producer).is_none() {
                        return Err(DocumentError::UnknownPersistentProducer(producer));
                    }
                }
            }
            SketchSupportRecipe::Origin | SketchSupportRecipe::PlanarFace { .. } => {
                return Err(DocumentError::SketchSupportMismatch);
            }
        }
        if feature.sketch_payload.as_ref() == Some(&payload) {
            return Ok(false);
        }
        let next_revision = record
            .geometry_revision
            .checked_add(1)
            .ok_or(DocumentError::SketchRevisionExhausted(sketch))?;
        if self.state.features.iter().any(|candidate| {
            candidate.outputs.contains(&FeatureOutput::Sketch {
                sketch,
                geometry_revision: next_revision,
            })
        }) {
            return Err(DocumentError::DuplicateSketchGeometryRevision {
                sketch,
                geometry_revision: next_revision,
            });
        }

        let previous = self.state.clone();
        let feature = &mut self.state.features[feature_index];
        feature.sketch_payload = Some(payload);
        feature.outputs[0] = FeatureOutput::Sketch {
            sketch,
            geometry_revision: next_revision,
        };
        self.state.sketches[sketch_index].geometry_revision = next_revision;
        self.mark_branch_dirty(feature_index);
        self.finish_user_edit(previous);
        Ok(true)
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[must_use]
    pub const fn undo_limit(&self) -> usize {
        self.undo_limit
    }

    pub fn set_undo_limit(&mut self, limit: usize) -> Result<(), DocumentError> {
        if limit > MAX_UNDO_LIMIT {
            return Err(DocumentError::UndoLimitExceeded { limit });
        }
        self.undo_limit = limit;
        trim_front(&mut self.undo, limit);
        trim_front(&mut self.redo, limit);
        Ok(())
    }

    pub fn clear_undo_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        let current = std::mem::replace(&mut self.state, previous);
        push_bounded(&mut self.redo, current, self.undo_limit);
        self.bump_revision();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        let current = std::mem::replace(&mut self.state, next);
        push_bounded(&mut self.undo, current, self.undo_limit);
        self.bump_revision();
        true
    }

    /// Appends a topologically valid node and all newly allocated outputs.
    ///
    /// IDs are monotonically allocated and are never reused, including after
    /// undo. Input-object producers are added to `dependencies` automatically.
    pub fn append_feature(
        &mut self,
        mut draft: FeatureDraft,
    ) -> Result<AppendFeatureResult, DocumentError> {
        if self.history_position() != self.state.features.len() {
            return Err(DocumentError::HistoryCursorNotAtEnd);
        }
        validate_replay_action(&draft.action)?;
        validate_label(&draft.label)?;
        validate_reference_count("inputs", draft.inputs.len())?;
        validate_reference_count("parameter inputs", draft.parameter_inputs.len())?;
        validate_reference_count("dependencies", draft.dependencies.len())?;
        validate_reference_count("outputs", draft.outputs.len())?;
        ensure_unique(draft.inputs.iter().copied(), "feature inputs")?;
        ensure_unique(
            draft.parameter_inputs.iter().copied(),
            "feature parameter inputs",
        )?;
        ensure_unique(draft.dependencies.iter().copied(), "feature dependencies")?;
        validate_action_feature_inputs(&draft.action, &draft.inputs)?;

        for parameter in &draft.parameter_inputs {
            if self.parameter(*parameter).is_none() {
                return Err(DocumentError::UnknownParameter(*parameter));
            }
        }
        validate_action_parameter_inputs(
            &draft.action,
            &draft.parameter_inputs,
            &self.state.parameters,
        )?;
        if let Some(component) = &draft.component_instance {
            component.validate()?;
            if draft.kind != FeatureKind::BaseBody
                || !draft.inputs.is_empty()
                || !draft.parameter_inputs.is_empty()
            {
                return Err(DocumentError::InvalidComponentFeature);
            }
            if self.state.component_instances.len() == components::MAX_COMPONENT_INSTANCES {
                return Err(DocumentError::CapacityExceeded {
                    resource: "component instances",
                    limit: components::MAX_COMPONENT_INSTANCES,
                });
            }
        }

        if self.state.features.len() == MAX_FEATURES {
            return Err(DocumentError::CapacityExceeded {
                resource: "features",
                limit: MAX_FEATURES,
            });
        }
        let feature_positions = self.feature_positions();
        for dependency in &draft.dependencies {
            if !feature_positions.contains_key(dependency) {
                return Err(DocumentError::UnknownFeature(*dependency));
            }
        }
        for input in &draft.inputs {
            let producer = match input {
                FeatureInput::Feature(feature) => {
                    if !feature_positions.contains_key(feature) {
                        return Err(DocumentError::UnknownFeature(*feature));
                    }
                    *feature
                }
                FeatureInput::Body(body) => {
                    self.body(*body)
                        .ok_or(DocumentError::UnknownBody(*body))?
                        .last_feature
                }
                FeatureInput::Sketch(sketch) => {
                    self.sketch(*sketch)
                        .ok_or(DocumentError::UnknownSketch(*sketch))?
                        .last_feature
                }
            };
            draft.dependencies.push(producer);
        }
        draft.dependencies.sort_by_key(|dependency| {
            feature_positions
                .get(dependency)
                .copied()
                .unwrap_or(usize::MAX)
        });
        draft.dependencies.dedup();
        validate_reference_count("canonical dependencies", draft.dependencies.len())?;

        let mut created_body_count = 0;
        let mut created_sketch_count = 0;
        let mut touched_outputs = BTreeSet::new();
        for output in &draft.outputs {
            match output {
                OutputDraft::CreateBody { label } => {
                    validate_label(label)?;
                    created_body_count += 1;
                }
                OutputDraft::ModifyBody(body) => {
                    let record = self.body(*body).ok_or(DocumentError::UnknownBody(*body))?;
                    if record.read_only {
                        return Err(DocumentError::ReadOnlyBody(*body));
                    }
                    if !draft.inputs.contains(&FeatureInput::Body(*body)) {
                        return Err(DocumentError::ModifiedObjectMustBeInput(
                            FeatureInput::Body(*body),
                        ));
                    }
                    if !touched_outputs.insert(FeatureInput::Body(*body)) {
                        return Err(DocumentError::DuplicateReference("feature outputs"));
                    }
                }
                OutputDraft::CreateSketch { label, .. } => {
                    validate_label(label)?;
                    created_sketch_count += 1;
                }
                OutputDraft::ModifySketch { sketch, .. } => {
                    let record = self
                        .sketch(*sketch)
                        .ok_or(DocumentError::UnknownSketch(*sketch))?;
                    if record.read_only {
                        return Err(DocumentError::ReadOnlySketch(*sketch));
                    }
                    if !draft.inputs.contains(&FeatureInput::Sketch(*sketch)) {
                        return Err(DocumentError::ModifiedObjectMustBeInput(
                            FeatureInput::Sketch(*sketch),
                        ));
                    }
                    if !touched_outputs.insert(FeatureInput::Sketch(*sketch)) {
                        return Err(DocumentError::DuplicateReference("feature outputs"));
                    }
                }
            }
        }
        if self.state.bodies.len() + created_body_count > MAX_OBJECTS_PER_KIND {
            return Err(DocumentError::CapacityExceeded {
                resource: "bodies",
                limit: MAX_OBJECTS_PER_KIND,
            });
        }
        if self.state.sketches.len() + created_sketch_count > MAX_OBJECTS_PER_KIND {
            return Err(DocumentError::CapacityExceeded {
                resource: "sketches",
                limit: MAX_OBJECTS_PER_KIND,
            });
        }
        if draft.component_instance.is_some()
            && (created_body_count == 0 || created_body_count != draft.outputs.len())
        {
            return Err(DocumentError::InvalidComponentFeature);
        }

        let existing_branches = draft
            .inputs
            .iter()
            .filter_map(|input| match input {
                FeatureInput::Body(body) => Some(*body),
                FeatureInput::Sketch(sketch) => {
                    self.sketch(*sketch).and_then(|record| record.support_body)
                }
                FeatureInput::Feature(_) => None,
            })
            .chain(draft.outputs.iter().filter_map(|output| match output {
                OutputDraft::ModifyBody(body) => Some(*body),
                OutputDraft::CreateBody { .. }
                | OutputDraft::CreateSketch { .. }
                | OutputDraft::ModifySketch { .. } => None,
            }))
            .collect::<BTreeSet<_>>();
        let branch_count = existing_branches.len() + created_body_count;
        let boolean_recipe = match &draft.action {
            ReplayAction::Boolean(recipe)
                if draft.kind == FeatureKind::Boolean
                    && created_body_count == 0
                    && draft.outputs == [OutputDraft::ModifyBody(recipe.target)]
                    && existing_branches == BTreeSet::from([recipe.target, recipe.tool]) =>
            {
                Some(*recipe)
            }
            ReplayAction::Boolean(_) => return Err(DocumentError::InvalidBooleanFeature),
            _ => None,
        };
        if branch_count > 1 && draft.component_instance.is_none() && boolean_recipe.is_none() {
            return Err(DocumentError::CrossBodyFeatureUnsupported);
        }
        self.validate_new_sketch_payload(&draft, &existing_branches)?;
        let primary_branch = boolean_recipe
            .map(|recipe| recipe.target)
            .or_else(|| existing_branches.first().copied());
        if draft.action != ReplayAction::Marker
            && let Some(body) = primary_branch
            && !draft.outputs.contains(&OutputDraft::ModifyBody(body))
        {
            return Err(DocumentError::BranchBodyMustBeOutput(body));
        }
        if let Some(committed) = draft.committed {
            if let Some(dependency) = draft.dependencies.iter().copied().find(|dependency| {
                self.feature(*dependency).is_some_and(|feature| {
                    feature.state.rebuild == RebuildState::Dirty
                        || feature.state.suppressed
                        || feature.committed.is_none()
                })
            }) {
                return Err(DocumentError::UnavailableDependency(dependency));
            }
            if draft.action == ReplayAction::Marker && committed.input != committed.output {
                return Err(DocumentError::MarkerChangedSnapshot);
            }
            let expected_input = if let Some(body) = primary_branch {
                self.body(body)
                    .and_then(|record| record.committed_snapshot)
                    .ok_or(DocumentError::BodyHasNoCommittedSnapshot(body))?
            } else if created_body_count >= 1 {
                SnapshotId::ZERO
            } else {
                committed.input
            };
            if committed.input != expected_input {
                return Err(DocumentError::SnapshotChainMismatch {
                    expected: expected_input,
                    actual: committed.input,
                });
            }
        }

        let feature_id = FeatureId::from_allocated(self.allocators.next_feature);
        let next_feature_allocator = checked_next(self.allocators.next_feature, "feature IDs")?;
        let next_body_allocator =
            checked_advance(self.allocators.next_body, created_body_count, "body IDs")?;
        let next_sketch_allocator = checked_advance(
            self.allocators.next_sketch,
            created_sketch_count,
            "sketch IDs",
        )?;
        let component_instance_id = draft
            .component_instance
            .as_ref()
            .map(|_| ComponentInstanceId::from_allocated(self.allocators.next_component_instance));
        let next_component_instance_allocator = if component_instance_id.is_some() {
            checked_next(
                self.allocators.next_component_instance,
                "component instance IDs",
            )?
        } else {
            self.allocators.next_component_instance
        };
        let mut next_body = self.allocators.next_body;
        let mut next_sketch = self.allocators.next_sketch;
        let sketch_support_body = existing_branches.first().copied().or_else(|| {
            (created_body_count == 1).then(|| BodyId::from_allocated(self.allocators.next_body))
        });
        let mut outputs = Vec::with_capacity(draft.outputs.len());
        let mut created_bodies = Vec::with_capacity(created_body_count);
        let mut created_sketches = Vec::with_capacity(created_sketch_count);
        let committed_snapshot = draft.committed.map(|commit| commit.output);
        let previous = self.state.clone();

        for output in draft.outputs {
            match output {
                OutputDraft::CreateBody { label } => {
                    let id = BodyId::from_allocated(next_body);
                    next_body += 1;
                    self.state.bodies.push(BodyRecord {
                        id,
                        label,
                        created_by: feature_id,
                        last_feature: feature_id,
                        visible: true,
                        read_only: false,
                        committed_snapshot,
                    });
                    outputs.push(FeatureOutput::Body(id));
                    created_bodies.push(id);
                }
                OutputDraft::ModifyBody(id) => {
                    let body = self
                        .state
                        .bodies
                        .iter_mut()
                        .find(|body| body.id == id)
                        .expect("validated body output must still exist");
                    body.last_feature = feature_id;
                    if let Some(snapshot) = committed_snapshot {
                        body.committed_snapshot = Some(snapshot);
                    }
                    outputs.push(FeatureOutput::Body(id));
                }
                OutputDraft::CreateSketch {
                    label,
                    geometry_revision,
                } => {
                    let id = SketchId::from_allocated(next_sketch);
                    next_sketch += 1;
                    self.state.sketches.push(SketchRecord {
                        id,
                        label,
                        created_by: feature_id,
                        last_feature: feature_id,
                        support_body: sketch_support_body,
                        geometry_revision,
                        visible: true,
                        auto_hidden_by: None,
                        read_only: false,
                        committed_snapshot,
                    });
                    outputs.push(FeatureOutput::Sketch {
                        sketch: id,
                        geometry_revision,
                    });
                    created_sketches.push(id);
                }
                OutputDraft::ModifySketch {
                    sketch: id,
                    geometry_revision,
                } => {
                    let sketch = self
                        .state
                        .sketches
                        .iter_mut()
                        .find(|sketch| sketch.id == id)
                        .expect("validated sketch output must still exist");
                    sketch.last_feature = feature_id;
                    sketch.geometry_revision = geometry_revision;
                    if let Some(snapshot) = committed_snapshot {
                        sketch.committed_snapshot = Some(snapshot);
                    }
                    outputs.push(FeatureOutput::Sketch {
                        sketch: id,
                        geometry_revision,
                    });
                }
            }
        }

        if let Some(recipe) = boolean_recipe
            && !recipe.keep_tool
            && let Some(tool) = self
                .state
                .bodies
                .iter_mut()
                .find(|body| body.id == recipe.tool)
        {
            tool.visible = false;
        }

        if let (Some(id), Some(component)) =
            (component_instance_id, draft.component_instance.take())
        {
            self.state
                .component_instances
                .push(ComponentInstanceRecord::from_draft(
                    id,
                    feature_id,
                    created_bodies.clone(),
                    component,
                )?);
        }

        self.state.features.push(FeatureNode {
            id: feature_id,
            kind: draft.kind,
            label: draft.label,
            inputs: draft.inputs,
            parameter_inputs: draft.parameter_inputs,
            component_instance: component_instance_id,
            sketch_payload: draft.sketch_payload,
            dependencies: draft.dependencies,
            outputs,
            action: draft.action,
            state: FeatureState {
                suppressed: false,
                read_only: draft.read_only,
                rebuild: if draft.committed.is_some() {
                    RebuildState::Clean
                } else {
                    RebuildState::Dirty
                },
            },
            committed: draft.committed,
        });
        self.state.history_cursor = HistoryCursor::End;
        if let Some(commit) = draft.committed {
            self.state.head_snapshot = Some(commit.output);
        }
        debug_assert_eq!(next_body, next_body_allocator);
        debug_assert_eq!(next_sketch, next_sketch_allocator);
        self.allocators.next_feature = next_feature_allocator;
        self.allocators.next_body = next_body_allocator;
        self.allocators.next_sketch = next_sketch_allocator;
        self.allocators.next_component_instance = next_component_instance_allocator;
        self.finish_user_edit(previous);

        Ok(AppendFeatureResult {
            feature: feature_id,
            created_bodies,
            created_sketches,
            created_component_instance: component_instance_id,
        })
    }

    pub fn rename_feature(
        &mut self,
        id: FeatureId,
        label: impl Into<String>,
    ) -> Result<bool, DocumentError> {
        let label = label.into();
        validate_label(&label)?;
        let index = self.feature_index(id)?;
        if self.state.features[index].state.read_only {
            return Err(DocumentError::ReadOnlyFeature(id));
        }
        if self.state.features[index].label == label {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.features[index].label = label;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn replace_feature_action(
        &mut self,
        id: FeatureId,
        action: ReplayAction,
    ) -> Result<bool, DocumentError> {
        validate_replay_action(&action)?;
        let index = self.feature_index(id)?;
        if self.state.features[index].state.read_only {
            return Err(DocumentError::ReadOnlyFeature(id));
        }
        validate_action_parameter_inputs(
            &action,
            &self.state.features[index].parameter_inputs,
            &self.state.parameters,
        )?;
        validate_action_feature_inputs(&action, &self.state.features[index].inputs)?;
        if self.state.features[index].action == action {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.features[index].action = action;
        self.mark_branch_dirty(index);
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_feature_suppressed(
        &mut self,
        id: FeatureId,
        suppressed: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.feature_index(id)?;
        if self.state.features[index].state.read_only {
            return Err(DocumentError::ReadOnlyFeature(id));
        }
        if self.state.features[index].state.suppressed == suppressed {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.features[index].state.suppressed = suppressed;
        self.mark_branch_dirty(index);
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_feature_read_only(
        &mut self,
        id: FeatureId,
        read_only: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.feature_index(id)?;
        if self.state.features[index].state.read_only == read_only {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.features[index].state.read_only = read_only;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_body_visible(&mut self, id: BodyId, visible: bool) -> Result<bool, DocumentError> {
        let index = self.body_index(id)?;
        if self.state.bodies[index].visible == visible {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.bodies[index].visible = visible;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_sketch_visible(
        &mut self,
        id: SketchId,
        visible: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.sketch_index(id)?;
        if self.state.sketches[index].visible == visible
            && self.state.sketches[index].auto_hidden_by.is_none()
        {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.sketches[index].visible = visible;
        self.state.sketches[index].auto_hidden_by = None;
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Coalesces the default auto-hide of a consumed sketch into the latest
    /// committed modeling feature. This deliberately creates no second undo
    /// checkpoint: undoing the consumer restores the sketch visibility from
    /// the document state immediately before that feature was appended.
    pub fn auto_hide_sketch_consumed_by(
        &mut self,
        sketch: SketchId,
        consumer: FeatureId,
    ) -> Result<bool, DocumentError> {
        let sketch_index = self.sketch_index(sketch)?;
        let feature = self
            .state
            .features
            .last()
            .filter(|feature| feature.id == consumer)
            .ok_or(DocumentError::UnavailableDependency(consumer))?;
        if feature.committed.is_none()
            || feature.state.suppressed
            || !matches!(
                feature.kind,
                FeatureKind::Extrude | FeatureKind::Add | FeatureKind::Cut
            )
            || !feature.inputs.contains(&FeatureInput::Sketch(sketch))
        {
            return Err(DocumentError::UnavailableDependency(consumer));
        }
        if !self.state.sketches[sketch_index].visible
            && self.state.sketches[sketch_index].auto_hidden_by == Some(consumer)
        {
            return Ok(false);
        }
        self.state.sketches[sketch_index].visible = false;
        self.state.sketches[sketch_index].auto_hidden_by = Some(consumer);
        Ok(true)
    }

    pub fn set_body_read_only(
        &mut self,
        id: BodyId,
        read_only: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.body_index(id)?;
        if self.state.bodies[index].read_only == read_only {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.bodies[index].read_only = read_only;
        self.finish_user_edit(previous);
        Ok(true)
    }

    pub fn set_sketch_read_only(
        &mut self,
        id: SketchId,
        read_only: bool,
    ) -> Result<bool, DocumentError> {
        let index = self.sketch_index(id)?;
        if self.state.sketches[index].read_only == read_only {
            return Ok(false);
        }
        let previous = self.state.clone();
        self.state.sketches[index].read_only = read_only;
        self.finish_user_edit(previous);
        Ok(true)
    }

    /// Produces a branch-replay plan from the first dirty feature.
    pub fn plan_rebuild(&self) -> Result<Option<RebuildPlan>, DocumentError> {
        let Some(feature) = self
            .active_features()
            .iter()
            .find(|feature| feature.state.rebuild == RebuildState::Dirty)
        else {
            return Ok(None);
        };
        self.plan_rebuild_from(feature.id).map(Some)
    }

    /// Produces a deterministic replay plan for the dependency branch rooted
    /// at `from`.
    ///
    /// Independent body roots are intentionally omitted. A later node enters
    /// the plan only when it depends on an already impacted node, so editing
    /// Body 1 never reconstructs Body 2 merely because its feature is later in
    /// the visual timeline.
    pub fn plan_rebuild_from(&self, from: FeatureId) -> Result<RebuildPlan, DocumentError> {
        let start = self.feature_index(from)?;
        let active_count = self.history_position();
        if start >= active_count {
            return Err(DocumentError::FeatureBeyondHistoryCursor(from));
        }
        let positions = self.feature_positions();
        let mut ancestors = BTreeSet::new();
        let mut pending_ancestors = self.state.features[start].dependencies.clone();
        while let Some(ancestor) = pending_ancestors.pop() {
            if ancestors.insert(ancestor) {
                let index = positions
                    .get(&ancestor)
                    .copied()
                    .ok_or(DocumentError::UnknownFeature(ancestor))?;
                pending_ancestors.extend_from_slice(&self.state.features[index].dependencies);
            }
        }
        if let Some(earlier) = self.state.features[..start].iter().find(|feature| {
            ancestors.contains(&feature.id) && feature.state.rebuild == RebuildState::Dirty
        }) {
            return Err(DocumentError::EarlierDirtyFeature(earlier.id));
        }

        let mut impacted = BTreeSet::from([from]);
        for feature in &self.state.features[start + 1..active_count] {
            if feature
                .dependencies
                .iter()
                .any(|dependency| impacted.contains(dependency))
            {
                impacted.insert(feature.id);
            }
        }
        let mut unavailable = BTreeMap::<FeatureId, bool>::new();
        let mut steps = Vec::with_capacity(impacted.len());
        for (timeline_index, feature) in self.state.features[..active_count].iter().enumerate() {
            let blocked_by = feature
                .dependencies
                .iter()
                .copied()
                .find(|dependency| unavailable.get(dependency).copied().unwrap_or(false));
            let disposition = if feature.state.suppressed {
                ReplayDisposition::Skip(SkipReason::ExplicitSuppression)
            } else if let Some(dependency) = blocked_by {
                ReplayDisposition::Skip(SkipReason::SuppressedDependency(dependency))
            } else {
                ReplayDisposition::Execute
            };
            unavailable.insert(feature.id, disposition.is_skipped());
            if impacted.contains(&feature.id) {
                let branches = feature_branches(feature, &self.state.sketches);
                if branches.len() > 1 && feature.component_instance.is_none() {
                    return Err(DocumentError::CrossBodyFeatureUnsupported);
                }
                steps.push(RebuildStep {
                    timeline_index,
                    feature: feature.id,
                    branches,
                    action: feature.action.clone(),
                    disposition,
                });
            }
        }

        let plan_branches = steps
            .iter()
            .flat_map(|step| step.branches.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut cursors = BTreeMap::<BodyId, SnapshotId>::new();
        for feature in &self.state.features[..start] {
            if let Some(commit) = feature.committed {
                for body in feature_branches(feature, &self.state.sketches) {
                    cursors.insert(body, commit.output);
                }
            }
        }
        let branch_bases = plan_branches
            .into_iter()
            .map(|body| {
                let snapshot = cursors.get(&body).copied();
                let replay_input = snapshot.or_else(|| {
                    let first_step = steps.iter().find(|step| step.branches.contains(&body));
                    let first_feature = first_step.and_then(|step| self.feature(step.feature));
                    if first_feature.is_some_and(|feature| {
                        self.body(body)
                            .is_some_and(|record| record.created_by == feature.id)
                    }) {
                        Some(SnapshotId::ZERO)
                    } else {
                        first_feature
                            .and_then(|feature| feature.committed)
                            .map(|commit| commit.input)
                    }
                });
                BranchSnapshot {
                    body,
                    snapshot,
                    replay_input,
                }
            })
            .collect();
        Ok(RebuildPlan {
            base_revision: self.revision,
            from,
            branch_bases,
            steps,
        })
    }

    pub fn begin_rebuild(&self, from: FeatureId) -> Result<RebuildTransaction, DocumentError> {
        Ok(RebuildTransaction {
            plan: self.plan_rebuild_from(from)?,
            base_state: self.state.clone(),
            successes: Vec::new(),
            failure: None,
        })
    }

    /// Atomically publishes every recorded replay result.
    ///
    /// Rebuild bookkeeping is derived state and therefore does not add a
    /// second user-facing undo entry. The recipe edit which made the branch dirty
    /// already owns the undo checkpoint.
    pub fn commit_rebuild(
        &mut self,
        transaction: RebuildTransaction,
    ) -> Result<RebuildCommit, DocumentError> {
        self.validate_transaction_revision(&transaction)?;
        if let Some(failure) = transaction.failure {
            return Err(DocumentError::RebuildFailed {
                feature: failure.feature,
                message: failure.message,
            });
        }
        let expected = transaction.plan.executable_count();
        if transaction.successes.len() != expected {
            return Err(DocumentError::RebuildIncomplete {
                completed: transaction.successes.len(),
                expected,
            });
        }

        let by_feature = transaction
            .successes
            .iter()
            .map(|success| (success.feature, success.association))
            .collect::<BTreeMap<_, _>>();
        let mut rebuilt = self.state.clone();
        let mut branch_heads = transaction
            .plan
            .branch_bases
            .iter()
            .map(|base| (base.body, base.snapshot))
            .collect::<BTreeMap<_, _>>();
        let mut latest_published = self.state.head_snapshot;
        for step in &transaction.plan.steps {
            let node = rebuilt.features.get_mut(step.timeline_index).ok_or(
                DocumentError::InvalidArchive("rebuild step points outside the feature timeline"),
            )?;
            if node.id != step.feature {
                return Err(DocumentError::InvalidArchive(
                    "rebuild step no longer matches the feature timeline",
                ));
            }
            node.state.rebuild = RebuildState::Clean;
            if step.disposition.is_skipped() {
                node.committed = None;
            } else {
                let association = by_feature.get(&step.feature).copied().ok_or(
                    DocumentError::RebuildIncomplete {
                        completed: transaction.successes.len(),
                        expected,
                    },
                )?;
                node.committed = Some(association);
                latest_published = Some(association.output);
                for body in &step.branches {
                    branch_heads.insert(*body, Some(association.output));
                }
                for output in &node.outputs {
                    match output {
                        FeatureOutput::Body(id) => {
                            if let Some(body) =
                                rebuilt.bodies.iter_mut().find(|body| body.id == *id)
                            {
                                body.committed_snapshot = Some(association.output);
                            }
                        }
                        FeatureOutput::Sketch {
                            sketch: id,
                            geometry_revision,
                        } => {
                            if let Some(sketch) =
                                rebuilt.sketches.iter_mut().find(|sketch| sketch.id == *id)
                            {
                                sketch.committed_snapshot = Some(association.output);
                                sketch.geometry_revision = *geometry_revision;
                            }
                        }
                    }
                }
            }
        }
        rebuilt.head_snapshot = latest_published;
        for (body_id, snapshot) in branch_heads {
            if let Some(body) = rebuilt.bodies.iter_mut().find(|body| body.id == body_id) {
                body.committed_snapshot = snapshot;
            }
        }
        let touched_sketches = transaction
            .plan
            .steps
            .iter()
            .filter_map(|step| rebuilt.features.get(step.timeline_index))
            .flat_map(|feature| feature.outputs.iter())
            .filter_map(|output| match output {
                FeatureOutput::Sketch { sketch, .. } => Some(*sketch),
                FeatureOutput::Body(_) => None,
            })
            .collect::<BTreeSet<_>>();
        let mut active_sketches = BTreeMap::<SketchId, (SnapshotId, u64)>::new();
        for feature in &rebuilt.features {
            if let Some(commit) = feature.committed {
                for output in &feature.outputs {
                    if let FeatureOutput::Sketch {
                        sketch,
                        geometry_revision,
                    } = output
                    {
                        active_sketches.insert(*sketch, (commit.output, *geometry_revision));
                    }
                }
            }
        }
        for sketch in rebuilt
            .sketches
            .iter_mut()
            .filter(|sketch| touched_sketches.contains(&sketch.id))
        {
            let active = active_sketches.get(&sketch.id).copied();
            sketch.committed_snapshot = active.map(|(snapshot, _)| snapshot);
            sketch.geometry_revision = active.map_or(0, |(_, revision)| revision);
        }
        reconcile_active_object_state(&mut rebuilt);
        let head_snapshot = rebuilt.head_snapshot;
        self.state = rebuilt;
        self.bump_revision();
        Ok(RebuildCommit {
            rebuilt_from: transaction.plan.from,
            completed_features: expected,
            head_snapshot,
            revision: self.revision,
        })
    }

    /// Consumes a rebuild transaction without changing the document.
    pub fn rollback_rebuild(
        &self,
        transaction: RebuildTransaction,
    ) -> Result<RebuildRollback, DocumentError> {
        self.validate_transaction_revision(&transaction)?;
        Ok(RebuildRollback {
            rebuilt_from: transaction.plan.from,
            discarded_results: transaction.successes.len(),
            failure: transaction.failure,
            retained_head: self.state.head_snapshot,
            revision: self.revision,
        })
    }

    #[must_use]
    pub fn to_native(&self) -> NativeDocument {
        NativeDocument {
            format: NATIVE_DOCUMENT_FORMAT.to_owned(),
            version: CURRENT_DOCUMENT_VERSION,
            revision: self.revision,
            undo_limit: self.undo_limit,
            allocators: self.allocators,
            state: self.state.clone(),
            legacy_sketch_payload_omissions: self
                .legacy_sketch_payload_omissions
                .iter()
                .copied()
                .collect(),
        }
    }

    pub fn from_native(mut native: NativeDocument) -> Result<Self, DocumentError> {
        if native.format != NATIVE_DOCUMENT_FORMAT {
            return Err(DocumentError::UnsupportedFormat(native.format));
        }
        if !(MIN_SUPPORTED_DOCUMENT_VERSION..=CURRENT_DOCUMENT_VERSION).contains(&native.version) {
            return Err(DocumentError::UnsupportedVersion {
                found: native.version,
                supported: CURRENT_DOCUMENT_VERSION,
            });
        }
        if native.revision == u64::MAX {
            return Err(DocumentError::InvalidArchive(
                "the reserved exhausted document revision is not loadable",
            ));
        }
        if native.undo_limit > MAX_UNDO_LIMIT {
            return Err(DocumentError::UndoLimitExceeded {
                limit: native.undo_limit,
            });
        }
        if native.version < EDITABLE_SKETCH_DOCUMENT_VERSION {
            for feature in &mut native.state.features {
                let Some(payload) = feature.sketch_payload.as_mut() else {
                    continue;
                };
                if payload.authoring.is_none() {
                    payload.authoring = Some(
                        artificer_sketch::SketchDefinition::from_legacy_profile(
                            &payload.profile,
                            artificer_protocol::PrecisionPolicy::default(),
                        )
                        .map_err(|_| {
                            DocumentError::InvalidArchive(
                                "a legacy sketch profile could not migrate to editable intent",
                            )
                        })?,
                    );
                }
            }
        } else if native.state.features.iter().any(|feature| {
            feature.kind == FeatureKind::Sketch
                && feature
                    .sketch_payload
                    .as_ref()
                    .is_some_and(|payload| payload.authoring.is_none())
        }) {
            return Err(DocumentError::InvalidArchive(
                "a v6 sketch payload is missing its editable authoring graph",
            ));
        }
        let legacy_sketch_payload_omissions = if native.version < PORTABLE_SKETCH_DOCUMENT_VERSION {
            native
                .state
                .features
                .iter()
                .filter(|feature| {
                    feature.kind == FeatureKind::Sketch && feature.sketch_payload.is_none()
                })
                .map(|feature| feature.id)
                .collect::<BTreeSet<_>>()
        } else {
            let omissions = native
                .legacy_sketch_payload_omissions
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            if omissions.len() != native.legacy_sketch_payload_omissions.len() {
                return Err(DocumentError::InvalidArchive(
                    "legacy sketch-payload omission IDs must be unique",
                ));
            }
            omissions
        };
        validate_loaded_state(
            &native.state,
            native.allocators,
            &legacy_sketch_payload_omissions,
        )?;
        Ok(Self {
            state: native.state,
            allocators: native.allocators,
            revision: native.revision,
            undo_limit: native.undo_limit,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            legacy_sketch_payload_omissions,
        })
    }

    fn feature_positions(&self) -> BTreeMap<FeatureId, usize> {
        self.state
            .features
            .iter()
            .enumerate()
            .map(|(index, feature)| (feature.id, index))
            .collect()
    }

    fn feature_index(&self, id: FeatureId) -> Result<usize, DocumentError> {
        self.state
            .features
            .iter()
            .position(|feature| feature.id == id)
            .ok_or(DocumentError::UnknownFeature(id))
    }

    fn body_index(&self, id: BodyId) -> Result<usize, DocumentError> {
        self.state
            .bodies
            .iter()
            .position(|body| body.id == id)
            .ok_or(DocumentError::UnknownBody(id))
    }

    fn sketch_index(&self, id: SketchId) -> Result<usize, DocumentError> {
        self.state
            .sketches
            .iter()
            .position(|sketch| sketch.id == id)
            .ok_or(DocumentError::UnknownSketch(id))
    }

    fn component_instance_index(&self, id: ComponentInstanceId) -> Result<usize, DocumentError> {
        self.state
            .component_instances
            .iter()
            .position(|component| component.id == id)
            .ok_or(DocumentError::UnknownComponentInstance(id))
    }

    fn joint_index(&self, id: JointId) -> Result<usize, DocumentError> {
        self.state
            .joints
            .iter()
            .position(|joint| joint.id == id)
            .ok_or(DocumentError::UnknownJoint(id))
    }

    fn validate_joint_edge(
        &self,
        parent: JointParent,
        child: ComponentInstanceId,
        replacing: Option<JointId>,
    ) -> Result<(), DocumentError> {
        if self.component_instance(child).is_none() {
            return Err(DocumentError::UnknownComponentInstance(child));
        }
        if let JointParent::Component(parent_component) = parent {
            if self.component_instance(parent_component).is_none() {
                return Err(DocumentError::UnknownComponentInstance(parent_component));
            }
            if parent_component == child {
                return Err(DocumentError::JointSelfCycle(child));
            }
        }
        if let Some(existing) = self
            .state
            .joints
            .iter()
            .find(|joint| joint.child == child && Some(joint.id) != replacing)
        {
            return Err(DocumentError::JointChildAlreadyParented {
                child,
                existing: existing.id,
            });
        }

        let mut cursor = match parent {
            JointParent::World => None,
            JointParent::Component(component) => Some(component),
        };
        let mut visited = BTreeSet::new();
        while let Some(component) = cursor {
            if component == child {
                return Err(DocumentError::JointCycle(child));
            }
            if !visited.insert(component) {
                return Err(DocumentError::JointCycle(child));
            }
            cursor = self
                .state
                .joints
                .iter()
                .find(|joint| joint.child == component && Some(joint.id) != replacing)
                .and_then(|joint| match joint.parent {
                    JointParent::World => None,
                    JointParent::Component(parent) => Some(parent),
                });
        }
        Ok(())
    }

    fn validate_new_sketch_payload(
        &self,
        draft: &FeatureDraft,
        branches: &BTreeSet<BodyId>,
    ) -> Result<(), DocumentError> {
        let payload = match (draft.kind, draft.sketch_payload.as_ref()) {
            (FeatureKind::Sketch, Some(payload)) => payload,
            (FeatureKind::Sketch, None) => return Err(DocumentError::SketchPayloadRequired),
            (_, Some(_)) => return Err(DocumentError::SketchPayloadOnNonSketchFeature),
            (_, None) => return Ok(()),
        };
        payload.validate()?;
        let sketch_outputs = draft
            .outputs
            .iter()
            .filter_map(|output| match output {
                OutputDraft::CreateSketch {
                    geometry_revision, ..
                } => Some((None, *geometry_revision)),
                OutputDraft::ModifySketch {
                    sketch,
                    geometry_revision,
                } => Some((Some(*sketch), *geometry_revision)),
                OutputDraft::CreateBody { .. } | OutputDraft::ModifyBody(_) => None,
            })
            .collect::<Vec<_>>();
        if draft.outputs.len() != 1 || sketch_outputs.len() != 1 {
            return Err(DocumentError::InvalidSketchPayloadOutput);
        }
        let (modified_sketch, geometry_revision) = sketch_outputs[0];
        if geometry_revision == 0 {
            return Err(DocumentError::InvalidSketchGeometryRevision);
        }
        if let Some(sketch) = modified_sketch
            && self.state.features.iter().any(|feature| {
                feature.outputs.contains(&FeatureOutput::Sketch {
                    sketch,
                    geometry_revision,
                })
            })
        {
            return Err(DocumentError::DuplicateSketchGeometryRevision {
                sketch,
                geometry_revision,
            });
        }

        match &payload.support {
            SketchSupportRecipe::Origin if !branches.is_empty() => {
                return Err(DocumentError::SketchSupportMismatch);
            }
            SketchSupportRecipe::Origin => {}
            SketchSupportRecipe::PlanarFace { body, face } => {
                if self.body(*body).is_none() {
                    return Err(DocumentError::UnknownBody(*body));
                }
                if branches.len() != 1 || !branches.contains(body) {
                    return Err(DocumentError::SketchSupportMismatch);
                }
                for producer in persistent_ref_producers(face) {
                    if self.feature(producer).is_none() {
                        return Err(DocumentError::UnknownPersistentProducer(producer));
                    }
                }
            }
        }
        Ok(())
    }

    fn mark_branch_dirty(&mut self, start: usize) {
        let mut impacted = BTreeSet::from([self.state.features[start].id]);
        for feature in &mut self.state.features[start..] {
            if impacted.contains(&feature.id)
                || feature
                    .dependencies
                    .iter()
                    .any(|dependency| impacted.contains(dependency))
            {
                impacted.insert(feature.id);
                feature.state.rebuild = RebuildState::Dirty;
            }
        }
    }

    fn mark_parameter_consumers_dirty(&mut self, affected: &BTreeSet<ParameterId>) {
        let starts = self
            .state
            .features
            .iter()
            .enumerate()
            .filter_map(|(index, feature)| {
                feature
                    .parameter_inputs
                    .iter()
                    .any(|parameter| affected.contains(parameter))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        for start in starts {
            self.mark_branch_dirty(start);
        }
    }

    fn finish_user_edit(&mut self, previous: DocumentState) {
        push_bounded(&mut self.undo, previous, self.undo_limit);
        self.redo.clear();
        self.bump_revision();
    }

    fn bump_revision(&mut self) {
        // `u64::MAX` is reserved as an invalid archive sentinel. State equality
        // is also checked on rebuild publication, so rollover cannot make a
        // stale transaction authoritative.
        self.revision = self
            .revision
            .checked_add(1)
            .filter(|revision| *revision != u64::MAX)
            .unwrap_or(0);
    }

    fn validate_transaction_revision(
        &self,
        transaction: &RebuildTransaction,
    ) -> Result<(), DocumentError> {
        if transaction.plan.base_revision != self.revision || transaction.base_state != self.state {
            return Err(DocumentError::StaleRebuild {
                expected_revision: transaction.plan.base_revision,
                actual_revision: self.revision,
            });
        }
        Ok(())
    }
}

/// Whether one planned node executes or is omitted from the rebuilt model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayDisposition {
    Execute,
    Skip(SkipReason),
}

impl ReplayDisposition {
    const fn is_skipped(self) -> bool {
        matches!(self, Self::Skip(_))
    }
}

/// Deterministic reason for omitting a replay node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    ExplicitSuppression,
    SuppressedDependency(FeatureId),
}

/// One immutable step in a rebuild plan.
#[derive(Clone, Debug, PartialEq)]
pub struct RebuildStep {
    pub timeline_index: usize,
    pub feature: FeatureId,
    /// Zero or one independent body root for the current bounded kernel.
    pub branches: Vec<BodyId>,
    pub action: ReplayAction,
    pub disposition: ReplayDisposition,
}

/// Snapshot immediately before replay starts on one independent body root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BranchSnapshot {
    pub body: BodyId,
    /// Existing body state before replay, or `None` for a root constructor.
    pub snapshot: Option<SnapshotId>,
    /// Expected first command input, including the empty snapshot for roots.
    pub replay_input: Option<SnapshotId>,
}

/// Deterministic dependency-branch replay generated from one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct RebuildPlan {
    pub base_revision: u64,
    pub from: FeatureId,
    pub branch_bases: Vec<BranchSnapshot>,
    pub steps: Vec<RebuildStep>,
}

impl RebuildPlan {
    pub fn executable_steps(&self) -> impl Iterator<Item = &RebuildStep> {
        self.steps
            .iter()
            .filter(|step| step.disposition == ReplayDisposition::Execute)
    }

    #[must_use]
    pub fn executable_count(&self) -> usize {
        self.executable_steps().count()
    }

    #[must_use]
    pub fn branch_base(&self, body: BodyId) -> Option<Option<SnapshotId>> {
        self.branch_bases
            .iter()
            .find(|base| base.body == body)
            .map(|base| base.snapshot)
    }

    /// Groups executable steps into deterministic waves whose body branches
    /// do not conflict. A rebuild executor may evaluate one wave in parallel,
    /// then publish its results in the original timeline order.
    #[must_use]
    pub fn parallel_waves(&self) -> Vec<Vec<&RebuildStep>> {
        let mut waves = Vec::<Vec<&RebuildStep>>::new();
        let mut branch_wave = BTreeMap::<BodyId, usize>::new();
        for step in self.executable_steps() {
            let wave_index = step
                .branches
                .iter()
                .filter_map(|body| branch_wave.get(body).copied())
                .max()
                .map_or(0, |wave| wave + 1);
            if waves.len() <= wave_index {
                waves.resize_with(wave_index + 1, Vec::new);
            }
            waves[wave_index].push(step);
            for body in &step.branches {
                branch_wave.insert(*body, wave_index);
            }
        }
        waves
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RebuildSuccess {
    feature: FeatureId,
    association: SnapshotAssociation,
}

/// Structured failure retained until an explicit rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebuildFailure {
    pub feature: FeatureId,
    pub message: String,
}

/// In-memory atomic rebuild journal.
#[derive(Clone, Debug)]
pub struct RebuildTransaction {
    plan: RebuildPlan,
    base_state: DocumentState,
    successes: Vec<RebuildSuccess>,
    failure: Option<RebuildFailure>,
}

impl RebuildTransaction {
    #[must_use]
    pub const fn plan(&self) -> &RebuildPlan {
        &self.plan
    }

    #[must_use]
    pub fn next_executable_step(&self) -> Option<&RebuildStep> {
        self.plan.executable_steps().nth(self.successes.len())
    }

    #[must_use]
    pub fn failure(&self) -> Option<&RebuildFailure> {
        self.failure.as_ref()
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failure.is_none() && self.successes.len() == self.plan.executable_count()
    }

    pub fn record_success(
        &mut self,
        feature: FeatureId,
        association: SnapshotAssociation,
    ) -> Result<(), DocumentError> {
        if let Some(failure) = &self.failure {
            return Err(DocumentError::RebuildFailed {
                feature: failure.feature,
                message: failure.message.clone(),
            });
        }
        let next_step = self
            .next_executable_step()
            .ok_or(DocumentError::RebuildAlreadyComplete)?;
        let expected = next_step.feature;
        let branches = next_step.branches.clone();
        let marker = next_step.action == ReplayAction::Marker;
        if feature != expected {
            return Err(DocumentError::RebuildOutOfOrder {
                expected,
                actual: feature,
            });
        }
        let mut cursors = self
            .plan
            .branch_bases
            .iter()
            .map(|base| (base.body, base.replay_input))
            .collect::<BTreeMap<_, _>>();
        for (step, success) in self.plan.executable_steps().zip(self.successes.iter()) {
            for body in &step.branches {
                cursors.insert(*body, Some(success.association.output));
            }
        }
        for body in &branches {
            if let Some(Some(expected_input)) = cursors.get(body).copied()
                && association.input != expected_input
            {
                return Err(DocumentError::SnapshotChainMismatch {
                    expected: expected_input,
                    actual: association.input,
                });
            }
        }
        if marker && association.input != association.output {
            return Err(DocumentError::MarkerChangedSnapshot);
        }
        self.successes.push(RebuildSuccess {
            feature,
            association,
        });
        Ok(())
    }

    pub fn record_failure(
        &mut self,
        feature: FeatureId,
        message: impl Into<String>,
    ) -> Result<(), DocumentError> {
        if self.failure.is_some() {
            return Err(DocumentError::RebuildFailureAlreadyRecorded);
        }
        let expected = self
            .next_executable_step()
            .ok_or(DocumentError::RebuildAlreadyComplete)?
            .feature;
        if feature != expected {
            return Err(DocumentError::RebuildOutOfOrder {
                expected,
                actual: feature,
            });
        }
        let message = message.into();
        if message.is_empty() || message.len() > MAX_LABEL_BYTES {
            return Err(DocumentError::InvalidFailureMessage);
        }
        self.failure = Some(RebuildFailure { feature, message });
        Ok(())
    }
}

/// Receipt returned after an atomic replay commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RebuildCommit {
    pub rebuilt_from: FeatureId,
    pub completed_features: usize,
    pub head_snapshot: Option<SnapshotId>,
    pub revision: u64,
}

/// Receipt proving that replay results were discarded without publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebuildRollback {
    pub rebuilt_from: FeatureId,
    pub discarded_results: usize,
    pub failure: Option<RebuildFailure>,
    pub retained_head: Option<SnapshotId>,
    pub revision: u64,
}

/// Structured document-layer rejection. Every error is non-mutating.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DocumentError {
    #[error("unknown feature {0}")]
    UnknownFeature(FeatureId),
    #[error("unknown body {0}")]
    UnknownBody(BodyId),
    #[error("unknown sketch {0}")]
    UnknownSketch(SketchId),
    #[error("unknown parameter {0}")]
    UnknownParameter(ParameterId),
    #[error("unknown component instance {0}")]
    UnknownComponentInstance(ComponentInstanceId),
    #[error("unknown assembly joint {0}")]
    UnknownJoint(JointId),
    #[error("feature {0} is read-only")]
    ReadOnlyFeature(FeatureId),
    #[error("body {0} is read-only")]
    ReadOnlyBody(BodyId),
    #[error("sketch {0} is read-only")]
    ReadOnlySketch(SketchId),
    #[error("parameter {parameter} is consumed by feature {feature}")]
    ParameterInUse {
        parameter: ParameterId,
        feature: FeatureId,
    },
    #[error(transparent)]
    Parameter(#[from] ParameterError),
    #[error(transparent)]
    ParameterizedKernel(#[from] ParameterizedKernelError),
    #[error(transparent)]
    Component(#[from] ComponentError),
    #[error(transparent)]
    Joint(#[from] JointError),
    #[error(transparent)]
    SketchPayload(#[from] SketchPayloadError),
    #[error(transparent)]
    SketchRegionRecipe(#[from] SketchRegionRecipeError),
    #[error("new sketch features require an exact portable sketch payload")]
    SketchPayloadRequired,
    #[error("only sketch features may carry a portable sketch payload")]
    SketchPayloadOnNonSketchFeature,
    #[error("a portable sketch payload must produce exactly one sketch revision")]
    InvalidSketchPayloadOutput,
    #[error("a portable sketch geometry revision must be non-zero")]
    InvalidSketchGeometryRevision,
    #[error("sketch {sketch} already contains geometry revision {geometry_revision}")]
    DuplicateSketchGeometryRevision {
        sketch: SketchId,
        geometry_revision: u64,
    },
    #[error("sketch {0} geometry revision space is exhausted")]
    SketchRevisionExhausted(SketchId),
    #[error("the portable sketch support does not match its feature body branch")]
    SketchSupportMismatch,
    #[error("portable sketch support references missing producer {0}")]
    UnknownPersistentProducer(FeatureId),
    #[error("a grounded component instance cannot be repositioned")]
    GroundedComponent(ComponentInstanceId),
    #[error("component {0} cannot be its own joint parent")]
    JointSelfCycle(ComponentInstanceId),
    #[error("component {child} already has parent joint {existing}")]
    JointChildAlreadyParented {
        child: ComponentInstanceId,
        existing: JointId,
    },
    #[error("joint parenting would create a cycle through component {0}")]
    JointCycle(ComponentInstanceId),
    #[error(
        "a component occurrence feature must create one or more new bodies and no other outputs"
    )]
    InvalidComponentFeature,
    #[error("{resource} exceeds the document limit of {limit}")]
    CapacityExceeded {
        resource: &'static str,
        limit: usize,
    },
    #[error("the document undo limit {limit} exceeds {MAX_UNDO_LIMIT}")]
    UndoLimitExceeded { limit: usize },
    #[error(
        "history position {position} is outside the timeline containing {feature_count} features"
    )]
    HistoryPositionOutOfRange {
        position: usize,
        feature_count: usize,
    },
    #[error("feature {0} is after the active history cursor")]
    FeatureBeyondHistoryCursor(FeatureId),
    #[error("new features can only be appended with the history cursor at the end")]
    HistoryCursorNotAtEnd,
    #[error("a document label must contain 1 to {MAX_LABEL_BYTES} bytes")]
    InvalidLabel,
    #[error("duplicate reference in {0}")]
    DuplicateReference(&'static str),
    #[error("a modified object must also be an input: {0:?}")]
    ModifiedObjectMustBeInput(FeatureInput),
    #[error("one feature cannot consume or modify more than one independent body root yet")]
    CrossBodyFeatureUnsupported,
    #[error("a Boolean feature must name two distinct body inputs and modify its target body")]
    InvalidBooleanFeature,
    #[error("document-only marker features cannot change the committed snapshot")]
    MarkerChangedSnapshot,
    #[error("entity-targeting kernel commands require a persistent target recipe")]
    PersistentTargetRequired,
    #[error("a targeted kernel replay must pair a face recipe with a supported face command")]
    InvalidTargetedKernel,
    #[error("sketch-region replay source {0} must be declared as a feature input")]
    SketchRegionSourceMustBeInput(SketchId),
    #[error("body {0} has no committed snapshot to use as a replay base")]
    BodyHasNoCommittedSnapshot(BodyId),
    #[error("body-changing feature must declare branch body {0} as an output")]
    BranchBodyMustBeOutput(BodyId),
    #[error("feature dependency {0} is dirty, suppressed, or has no committed association")]
    UnavailableDependency(FeatureId),
    #[error("snapshot chain mismatch: expected {expected}, received {actual}")]
    SnapshotChainMismatch {
        expected: SnapshotId,
        actual: SnapshotId,
    },
    #[error("feature {0} is dirty before the requested rebuild start")]
    EarlierDirtyFeature(FeatureId),
    #[error(
        "stale rebuild transaction: based on revision {expected_revision}, document is revision {actual_revision}"
    )]
    StaleRebuild {
        expected_revision: u64,
        actual_revision: u64,
    },
    #[error("rebuild failed at {feature}: {message}")]
    RebuildFailed { feature: FeatureId, message: String },
    #[error("rebuild is incomplete: completed {completed} of {expected} executable steps")]
    RebuildIncomplete { completed: usize, expected: usize },
    #[error("rebuild result is out of order: expected {expected}, received {actual}")]
    RebuildOutOfOrder {
        expected: FeatureId,
        actual: FeatureId,
    },
    #[error("the rebuild transaction is already complete")]
    RebuildAlreadyComplete,
    #[error("the rebuild transaction already contains a failure")]
    RebuildFailureAlreadyRecorded,
    #[error("a rebuild failure message must contain 1 to {MAX_LABEL_BYTES} bytes")]
    InvalidFailureMessage,
    #[error("unsupported native document format {0:?}")]
    UnsupportedFormat(String),
    #[error("unsupported native document version {found}; this build supports {supported}")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("invalid native document: {0}")]
    InvalidArchive(&'static str),
    #[error("stable {0} exhausted")]
    IdExhausted(&'static str),
}

fn validate_label(label: &str) -> Result<(), DocumentError> {
    if label.is_empty() || label.len() > MAX_LABEL_BYTES {
        Err(DocumentError::InvalidLabel)
    } else {
        Ok(())
    }
}

fn validate_replay_action(action: &ReplayAction) -> Result<(), DocumentError> {
    match action {
        ReplayAction::Kernel(
            KernelCommand::ExtrudeFaceProfile { .. }
            | KernelCommand::ExtrudeFacePlanarProfile { .. }
            | KernelCommand::PushPullFace { .. }
            | KernelCommand::DrillHole { .. }
            | KernelCommand::AddRib { .. }
            | KernelCommand::FinishEdge { .. },
        ) => Err(DocumentError::PersistentTargetRequired),
        ReplayAction::TargetedKernel(targeted) if targeted.validate().is_err() => {
            Err(DocumentError::InvalidTargetedKernel)
        }
        ReplayAction::ParameterizedKernel(recipe) => recipe.validate().map_err(Into::into),
        ReplayAction::SketchRegionExtrusion(recipe) => recipe.validate().map_err(Into::into),
        ReplayAction::Boolean(recipe) if recipe.target == recipe.tool => {
            Err(DocumentError::InvalidBooleanFeature)
        }
        ReplayAction::Marker
        | ReplayAction::Kernel(_)
        | ReplayAction::TargetedKernel(_)
        | ReplayAction::Boolean(_) => Ok(()),
    }
}

fn validate_action_parameter_inputs(
    action: &ReplayAction,
    parameter_inputs: &[ParameterId],
    parameters: &ParameterTable,
) -> Result<(), DocumentError> {
    if let ReplayAction::ParameterizedKernel(recipe) = action {
        recipe.validate_parameter_inputs(parameter_inputs, parameters)?;
    }
    Ok(())
}

fn validate_action_feature_inputs(
    action: &ReplayAction,
    feature_inputs: &[FeatureInput],
) -> Result<(), DocumentError> {
    if let ReplayAction::SketchRegionExtrusion(recipe) = action
        && !feature_inputs.contains(&FeatureInput::Sketch(recipe.sketch))
    {
        return Err(DocumentError::SketchRegionSourceMustBeInput(recipe.sketch));
    }
    if let ReplayAction::Boolean(recipe) = action
        && (!feature_inputs.contains(&FeatureInput::Body(recipe.target))
            || !feature_inputs.contains(&FeatureInput::Body(recipe.tool)))
    {
        return Err(DocumentError::InvalidBooleanFeature);
    }
    Ok(())
}

fn validate_all_action_parameter_inputs(state: &DocumentState) -> Result<(), DocumentError> {
    for feature in &state.features {
        validate_action_parameter_inputs(
            &feature.action,
            &feature.parameter_inputs,
            &state.parameters,
        )?;
    }
    Ok(())
}

fn validate_reference_count(resource: &'static str, count: usize) -> Result<(), DocumentError> {
    if count > MAX_NODE_REFERENCES {
        Err(DocumentError::CapacityExceeded {
            resource,
            limit: MAX_NODE_REFERENCES,
        })
    } else {
        Ok(())
    }
}

fn ensure_unique<T>(
    values: impl IntoIterator<Item = T>,
    resource: &'static str,
) -> Result<(), DocumentError>
where
    T: Ord,
{
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(DocumentError::DuplicateReference(resource));
        }
    }
    Ok(())
}

fn checked_next(value: u64, resource: &'static str) -> Result<u64, DocumentError> {
    value
        .checked_add(1)
        .filter(|next| *next != 0)
        .ok_or(DocumentError::IdExhausted(resource))
}

fn checked_advance(value: u64, count: usize, resource: &'static str) -> Result<u64, DocumentError> {
    value
        .checked_add(count as u64)
        .filter(|next| *next != 0)
        .ok_or(DocumentError::IdExhausted(resource))
}

fn history_cursor_position(
    features: &[FeatureNode],
    cursor: HistoryCursor,
) -> Result<usize, DocumentError> {
    match cursor {
        HistoryCursor::Start => Ok(0),
        HistoryCursor::End => Ok(features.len()),
        HistoryCursor::After(feature) => features
            .iter()
            .position(|node| node.id == feature)
            .map(|index| index + 1)
            .ok_or(DocumentError::UnknownFeature(feature)),
    }
}

fn active_feature_count(state: &DocumentState) -> Result<usize, DocumentError> {
    history_cursor_position(&state.features, state.history_cursor)
}

/// Recomputes the effective Browser associations at the persisted history
/// boundary without discarding cached associations on rolled-back features.
fn reconcile_active_object_state(state: &mut DocumentState) {
    let active_count = active_feature_count(state)
        .expect("document mutation must preserve a validated history cursor");
    let mut body_snapshots = state
        .bodies
        .iter()
        .map(|body| (body.id, None))
        .collect::<BTreeMap<_, _>>();
    let mut sketch_states = state
        .sketches
        .iter()
        .map(|sketch| (sketch.id, (None, 0)))
        .collect::<BTreeMap<_, _>>();
    let mut head_snapshot = None;

    for feature in &state.features[..active_count] {
        if let Some(commit) = feature.committed {
            let branches = feature_branches(feature, &state.sketches);
            let association_is_attached = branches.iter().all(|body| {
                let expected_input = state
                    .bodies
                    .iter()
                    .find(|record| record.id == *body)
                    .and_then(|record| {
                        if record.created_by == feature.id {
                            Some(SnapshotId::ZERO)
                        } else {
                            body_snapshots.get(body).copied().flatten()
                        }
                    });
                expected_input == Some(commit.input)
            });
            if association_is_attached {
                head_snapshot = Some(commit.output);
                for body in branches {
                    body_snapshots.insert(body, Some(commit.output));
                }
                for output in &feature.outputs {
                    if let FeatureOutput::Sketch {
                        sketch,
                        geometry_revision,
                    } = output
                    {
                        sketch_states.insert(*sketch, (Some(commit.output), *geometry_revision));
                    }
                }
            }
        }
        if feature.state.rebuild == RebuildState::Dirty {
            for output in &feature.outputs {
                if let FeatureOutput::Sketch {
                    sketch,
                    geometry_revision,
                } = output
                    && let Some(active) = sketch_states.get_mut(sketch)
                {
                    active.1 = *geometry_revision;
                }
            }
        }
    }

    for body in &mut state.bodies {
        body.committed_snapshot = body_snapshots.get(&body.id).copied().flatten();
    }
    for sketch in &mut state.sketches {
        let active = sketch_states.get(&sketch.id).copied().unwrap_or((None, 0));
        sketch.committed_snapshot = active.0;
        sketch.geometry_revision = active.1;
    }
    state.head_snapshot = head_snapshot;
}

fn feature_branches(feature: &FeatureNode, sketches: &[SketchRecord]) -> Vec<BodyId> {
    if let ReplayAction::Boolean(recipe) = &feature.action {
        return vec![recipe.target];
    }
    feature
        .inputs
        .iter()
        .filter_map(|input| match input {
            FeatureInput::Body(body) => Some(*body),
            FeatureInput::Sketch(sketch) => sketches
                .iter()
                .find(|record| record.id == *sketch)
                .and_then(|record| record.support_body),
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

fn persistent_ref_producers(reference: &persistent::PersistentRef) -> Vec<FeatureId> {
    let mut producers = Vec::new();
    let mut current = Some(reference);
    while let Some(reference) = current {
        producers.push(reference.producer);
        current = reference.lineage.as_deref();
    }
    producers
}

fn validate_loaded_sketch_payload(
    feature: &FeatureNode,
    feature_index: usize,
    positions: &BTreeMap<FeatureId, usize>,
    branches: &[BodyId],
    body_records: &BTreeMap<BodyId, &BodyRecord>,
    sketch_records: &BTreeMap<SketchId, &SketchRecord>,
    legacy_sketch_payload_omissions: &BTreeSet<FeatureId>,
) -> Result<(), DocumentError> {
    let payload = match (feature.kind, feature.sketch_payload.as_ref()) {
        (FeatureKind::Sketch, Some(payload)) => payload,
        (FeatureKind::Sketch, None) if legacy_sketch_payload_omissions.contains(&feature.id) => {
            return Ok(());
        }
        (FeatureKind::Sketch, None) => {
            return Err(DocumentError::InvalidArchive(
                "a v4 sketch feature is missing its portable payload",
            ));
        }
        (_, Some(_)) => {
            return Err(DocumentError::InvalidArchive(
                "a non-sketch feature carries a portable sketch payload",
            ));
        }
        (_, None) => return Ok(()),
    };

    payload.validate()?;
    let [
        FeatureOutput::Sketch {
            sketch,
            geometry_revision,
        },
    ] = feature.outputs.as_slice()
    else {
        return Err(DocumentError::InvalidArchive(
            "a portable sketch payload must produce exactly one sketch revision",
        ));
    };
    if *geometry_revision == 0 {
        return Err(DocumentError::InvalidArchive(
            "a portable sketch geometry revision must be non-zero",
        ));
    }
    let sketch_record = sketch_records
        .get(sketch)
        .ok_or(DocumentError::InvalidArchive(
            "a portable sketch payload references a missing sketch",
        ))?;

    match &payload.support {
        SketchSupportRecipe::Origin => {
            if !branches.is_empty() || sketch_record.support_body.is_some() {
                return Err(DocumentError::InvalidArchive(
                    "an origin sketch payload is attached to a body branch",
                ));
            }
        }
        SketchSupportRecipe::PlanarFace { body, face } => {
            if !body_records.contains_key(body)
                || branches != [*body]
                || sketch_record.support_body != Some(*body)
            {
                return Err(DocumentError::InvalidArchive(
                    "a planar-face sketch payload disagrees with its body branch",
                ));
            }
            for producer in persistent_ref_producers(face) {
                if positions
                    .get(&producer)
                    .is_none_or(|producer_index| *producer_index >= feature_index)
                {
                    return Err(DocumentError::InvalidArchive(
                        "a planar-face support producer must precede its sketch",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn push_bounded<T>(journal: &mut VecDeque<T>, value: T, limit: usize) {
    if limit == 0 {
        return;
    }
    if journal.len() == limit {
        journal.pop_front();
    }
    journal.push_back(value);
}

fn trim_front<T>(journal: &mut VecDeque<T>, limit: usize) {
    while journal.len() > limit {
        journal.pop_front();
    }
}

fn validate_loaded_joint_graph(state: &DocumentState) -> Result<(), DocumentError> {
    let components = state
        .component_instances
        .iter()
        .map(|component| component.id)
        .collect::<BTreeSet<_>>();
    let mut parent_joint_by_child = BTreeMap::<ComponentInstanceId, &JointRecord>::new();
    for joint in &state.joints {
        joint.validate()?;
        if !components.contains(&joint.child) {
            return Err(DocumentError::InvalidArchive(
                "an assembly joint child references a missing component",
            ));
        }
        if let JointParent::Component(parent) = joint.parent {
            if !components.contains(&parent) {
                return Err(DocumentError::InvalidArchive(
                    "an assembly joint parent references a missing component",
                ));
            }
            if parent == joint.child {
                return Err(DocumentError::InvalidArchive(
                    "an assembly joint cannot parent a component to itself",
                ));
            }
        }
        if parent_joint_by_child.insert(joint.child, joint).is_some() {
            return Err(DocumentError::InvalidArchive(
                "a component occurrence has more than one parent joint",
            ));
        }
    }

    for child in parent_joint_by_child.keys().copied() {
        let mut cursor = Some(child);
        let mut traversed = 0_usize;
        while let Some(component) = cursor {
            traversed += 1;
            if traversed > state.joints.len() + 1 {
                return Err(DocumentError::InvalidArchive(
                    "the assembly joint hierarchy contains a cycle",
                ));
            }
            cursor = parent_joint_by_child
                .get(&component)
                .and_then(|joint| match joint.parent {
                    JointParent::World => None,
                    JointParent::Component(parent) => Some(parent),
                });
        }
    }
    Ok(())
}

fn validate_loaded_state(
    state: &DocumentState,
    allocators: AllocatorState,
    legacy_sketch_payload_omissions: &BTreeSet<FeatureId>,
) -> Result<(), DocumentError> {
    if state.features.len() > MAX_FEATURES {
        return Err(DocumentError::CapacityExceeded {
            resource: "features",
            limit: MAX_FEATURES,
        });
    }
    if state.bodies.len() > MAX_OBJECTS_PER_KIND {
        return Err(DocumentError::CapacityExceeded {
            resource: "bodies",
            limit: MAX_OBJECTS_PER_KIND,
        });
    }
    if state.sketches.len() > MAX_OBJECTS_PER_KIND {
        return Err(DocumentError::CapacityExceeded {
            resource: "sketches",
            limit: MAX_OBJECTS_PER_KIND,
        });
    }
    if state.component_instances.len() > components::MAX_COMPONENT_INSTANCES {
        return Err(DocumentError::CapacityExceeded {
            resource: "component instances",
            limit: components::MAX_COMPONENT_INSTANCES,
        });
    }
    if state.joints.len() > assembly::MAX_JOINTS {
        return Err(DocumentError::CapacityExceeded {
            resource: "assembly joints",
            limit: assembly::MAX_JOINTS,
        });
    }
    if allocators.next_feature == 0
        || allocators.next_body == 0
        || allocators.next_sketch == 0
        || allocators.next_parameter == 0
        || allocators.next_component_instance == 0
        || allocators.next_joint == 0
    {
        return Err(DocumentError::InvalidArchive(
            "stable ID allocators must be non-zero",
        ));
    }

    state.parameters.validate()?;

    ensure_unique(
        state.features.iter().map(|feature| feature.id),
        "feature IDs",
    )?;
    ensure_unique(state.bodies.iter().map(|body| body.id), "body IDs")?;
    ensure_unique(state.sketches.iter().map(|sketch| sketch.id), "sketch IDs")?;
    ensure_unique(
        state
            .component_instances
            .iter()
            .map(|component| component.id),
        "component instance IDs",
    )?;
    ensure_unique(state.joints.iter().map(|joint| joint.id), "joint IDs")?;
    let positions = state
        .features
        .iter()
        .enumerate()
        .map(|(index, feature)| (feature.id, index))
        .collect::<BTreeMap<_, _>>();
    for feature_id in legacy_sketch_payload_omissions {
        let Some(feature) = state
            .features
            .iter()
            .find(|feature| feature.id == *feature_id)
        else {
            return Err(DocumentError::InvalidArchive(
                "a legacy sketch-payload omission references a missing feature",
            ));
        };
        if feature.kind != FeatureKind::Sketch || feature.sketch_payload.is_some() {
            return Err(DocumentError::InvalidArchive(
                "legacy sketch-payload omissions must reference payload-less sketch features",
            ));
        }
    }
    active_feature_count(state).map_err(|_| {
        DocumentError::InvalidArchive("the history cursor references a missing feature")
    })?;
    if positions.keys().any(|id| id.get() == 0)
        || state.bodies.iter().any(|body| body.id.get() == 0)
        || state.sketches.iter().any(|sketch| sketch.id.get() == 0)
        || state
            .component_instances
            .iter()
            .any(|component| component.id.get() == 0)
        || state.joints.iter().any(|joint| joint.id.get() == 0)
    {
        return Err(DocumentError::InvalidArchive(
            "stable document IDs must be non-zero",
        ));
    }
    let max_feature = positions.keys().map(|id| id.get()).max().unwrap_or(0);
    let max_body = state
        .bodies
        .iter()
        .map(|body| body.id.get())
        .max()
        .unwrap_or(0);
    let max_sketch = state
        .sketches
        .iter()
        .map(|sketch| sketch.id.get())
        .max()
        .unwrap_or(0);
    let max_parameter = state
        .parameters
        .records()
        .iter()
        .map(|parameter| parameter.id.get())
        .max()
        .unwrap_or(0);
    let max_component_instance = state
        .component_instances
        .iter()
        .map(|component| component.id.get())
        .max()
        .unwrap_or(0);
    let max_joint = state
        .joints
        .iter()
        .map(|joint| joint.id.get())
        .max()
        .unwrap_or(0);
    if allocators.next_feature <= max_feature
        || allocators.next_body <= max_body
        || allocators.next_sketch <= max_sketch
        || allocators.next_parameter <= max_parameter
        || allocators.next_component_instance <= max_component_instance
        || allocators.next_joint <= max_joint
    {
        return Err(DocumentError::InvalidArchive(
            "stable ID allocators must be above every retained ID",
        ));
    }

    let body_records = state
        .bodies
        .iter()
        .map(|body| (body.id, body))
        .collect::<BTreeMap<_, _>>();
    let sketch_records = state
        .sketches
        .iter()
        .map(|sketch| (sketch.id, sketch))
        .collect::<BTreeMap<_, _>>();
    let component_records = state
        .component_instances
        .iter()
        .map(|component| (component.id, component))
        .collect::<BTreeMap<_, _>>();
    let mut component_bodies = BTreeSet::new();
    for component in &state.component_instances {
        component.validate()?;
        ensure_unique(
            component.bodies.iter().copied(),
            "component instance bodies",
        )?;
        let feature = state
            .features
            .iter()
            .find(|feature| feature.id == component.created_by)
            .ok_or(DocumentError::InvalidArchive(
                "a component instance references a missing creating feature",
            ))?;
        if feature.component_instance != Some(component.id) {
            return Err(DocumentError::InvalidArchive(
                "component and creating feature associations are inconsistent",
            ));
        }
        for body in &component.bodies {
            let body_record = body_records.get(body).ok_or(DocumentError::InvalidArchive(
                "a component instance references a missing body",
            ))?;
            if body_record.created_by != component.created_by
                || !feature.outputs.contains(&FeatureOutput::Body(*body))
            {
                return Err(DocumentError::InvalidArchive(
                    "a component instance body is not created by its feature",
                ));
            }
            if !component_bodies.insert(*body) {
                return Err(DocumentError::InvalidArchive(
                    "one body cannot belong to multiple component instances",
                ));
            }
        }
    }
    validate_loaded_joint_graph(state)?;
    if state.sketches.iter().any(|sketch| {
        sketch
            .support_body
            .is_some_and(|body| !body_records.contains_key(&body))
    }) {
        return Err(DocumentError::InvalidArchive(
            "a sketch support references a missing body",
        ));
    }
    let mut seen_bodies = BTreeMap::<BodyId, FeatureId>::new();
    let mut seen_sketches = BTreeMap::<SketchId, FeatureId>::new();
    let mut seen_sketch_revisions = BTreeSet::<(SketchId, u64)>::new();
    let mut branch_snapshots = BTreeMap::<BodyId, Option<SnapshotId>>::new();
    let mut sketch_states = BTreeMap::<SketchId, (Option<SnapshotId>, u64)>::new();

    for (index, feature) in state.features.iter().enumerate() {
        if let Some(component) = feature.component_instance {
            let record = component_records
                .get(&component)
                .ok_or(DocumentError::InvalidArchive(
                    "a feature references a missing component instance",
                ))?;
            if feature.kind != FeatureKind::BaseBody
                || !feature.inputs.is_empty()
                || !feature.parameter_inputs.is_empty()
                || record.created_by != feature.id
                || feature.outputs.len() != record.bodies.len()
                || feature.outputs.iter().any(|output| {
                    !matches!(output, FeatureOutput::Body(body) if record.bodies.contains(body))
                })
            {
                return Err(DocumentError::InvalidArchive(
                    "a component creation feature has invalid inputs or ownership",
                ));
            }
        }
        validate_reference_count("parameter inputs", feature.parameter_inputs.len())?;
        ensure_unique(
            feature.parameter_inputs.iter().copied(),
            "feature parameter inputs",
        )?;
        if feature
            .parameter_inputs
            .iter()
            .any(|parameter| state.parameters.get(*parameter).is_none())
        {
            return Err(DocumentError::InvalidArchive(
                "a feature references a missing parameter",
            ));
        }
        validate_replay_action(&feature.action).map_err(|error| match error {
            DocumentError::PersistentTargetRequired => DocumentError::InvalidArchive(
                "raw entity-targeting commands require a persistent target recipe",
            ),
            DocumentError::InvalidTargetedKernel => DocumentError::InvalidArchive(
                "a targeted replay contains an invalid command/recipe pairing",
            ),
            _ => DocumentError::InvalidArchive("a replay action is invalid"),
        })?;
        validate_action_feature_inputs(&feature.action, &feature.inputs).map_err(|_| {
            DocumentError::InvalidArchive(
                "a sketch-region replay source must be a declared sketch input",
            )
        })?;
        validate_action_parameter_inputs(
            &feature.action,
            &feature.parameter_inputs,
            &state.parameters,
        )
        .map_err(|_| {
            DocumentError::InvalidArchive(
                "parameterized replay inputs or declared parameter types are invalid",
            )
        })?;
        validate_label(&feature.label)?;
        validate_reference_count("inputs", feature.inputs.len())?;
        validate_reference_count("dependencies", feature.dependencies.len())?;
        validate_reference_count("outputs", feature.outputs.len())?;
        ensure_unique(feature.inputs.iter().copied(), "feature inputs")?;
        ensure_unique(feature.dependencies.iter().copied(), "feature dependencies")?;
        ensure_unique(
            feature.outputs.iter().copied().map(FeatureOutput::object),
            "feature outputs",
        )?;
        for dependency in &feature.dependencies {
            if positions
                .get(dependency)
                .is_none_or(|position| *position >= index)
            {
                return Err(DocumentError::InvalidArchive(
                    "a feature dependency must precede its consumer",
                ));
            }
        }
        for input in &feature.inputs {
            let producer = match input {
                FeatureInput::Feature(id) => {
                    if positions.get(id).is_none_or(|position| *position >= index) {
                        return Err(DocumentError::InvalidArchive(
                            "a feature input must precede its consumer",
                        ));
                    }
                    *id
                }
                FeatureInput::Body(id) => {
                    seen_bodies
                        .get(id)
                        .copied()
                        .ok_or(DocumentError::InvalidArchive(
                            "a body input must already exist",
                        ))?
                }
                FeatureInput::Sketch(id) => {
                    seen_sketches
                        .get(id)
                        .copied()
                        .ok_or(DocumentError::InvalidArchive(
                            "a sketch input must already exist",
                        ))?
                }
            };
            if !feature.dependencies.contains(&producer) {
                return Err(DocumentError::InvalidArchive(
                    "every stable input producer must be a feature dependency",
                ));
            }
        }
        for output in &feature.outputs {
            match output {
                FeatureOutput::Body(id) => {
                    let record = body_records.get(id).ok_or(DocumentError::InvalidArchive(
                        "a feature output references a missing body",
                    ))?;
                    if record.created_by == feature.id {
                        if seen_bodies.insert(*id, feature.id).is_some() {
                            return Err(DocumentError::InvalidArchive(
                                "a body is created more than once",
                            ));
                        }
                        branch_snapshots.insert(*id, None);
                    } else if !seen_bodies.contains_key(id) {
                        return Err(DocumentError::InvalidArchive(
                            "a body modification precedes body creation",
                        ));
                    } else {
                        if !feature.inputs.contains(&FeatureInput::Body(*id)) {
                            return Err(DocumentError::InvalidArchive(
                                "a body modification must declare that body as an input",
                            ));
                        }
                        seen_bodies.insert(*id, feature.id);
                    }
                }
                FeatureOutput::Sketch {
                    sketch: id,
                    geometry_revision,
                } => {
                    if !seen_sketch_revisions.insert((*id, *geometry_revision)) {
                        return Err(DocumentError::InvalidArchive(
                            "a sketch geometry revision is produced more than once",
                        ));
                    }
                    let record = sketch_records.get(id).ok_or(DocumentError::InvalidArchive(
                        "a feature output references a missing sketch",
                    ))?;
                    if record.created_by == feature.id {
                        if seen_sketches.insert(*id, feature.id).is_some() {
                            return Err(DocumentError::InvalidArchive(
                                "a sketch is created more than once",
                            ));
                        }
                        sketch_states.insert(*id, (None, 0));
                    } else if !seen_sketches.contains_key(id) {
                        return Err(DocumentError::InvalidArchive(
                            "a sketch modification precedes sketch creation",
                        ));
                    } else {
                        if !feature.inputs.contains(&FeatureInput::Sketch(*id)) {
                            return Err(DocumentError::InvalidArchive(
                                "a sketch modification must declare that sketch as an input",
                            ));
                        }
                        seen_sketches.insert(*id, feature.id);
                    }
                }
            }
        }
        let branches = feature_branches(feature, &state.sketches);
        if branches.len() > 1 && feature.component_instance.is_none() {
            return Err(DocumentError::CrossBodyFeatureUnsupported);
        }
        validate_loaded_sketch_payload(
            feature,
            index,
            &positions,
            &branches,
            &body_records,
            &sketch_records,
            legacy_sketch_payload_omissions,
        )?;
        let support_body = branches.first().copied();
        if feature.outputs.iter().any(|output| {
            let FeatureOutput::Sketch { sketch, .. } = output else {
                return false;
            };
            sketch_records.get(sketch).is_some_and(|record| {
                record.created_by == feature.id && record.support_body != support_body
            })
        }) {
            return Err(DocumentError::InvalidArchive(
                "a sketch support body does not match its creating feature branch",
            ));
        }
        if feature.action != ReplayAction::Marker
            && let Some(body) = branches.first().copied()
            && !feature.outputs.contains(&FeatureOutput::Body(body))
        {
            return Err(DocumentError::InvalidArchive(
                "a body-changing feature must declare its branch body as an output",
            ));
        }
        if let Some(commit) = feature.committed {
            if feature.action == ReplayAction::Marker && commit.input != commit.output {
                return Err(DocumentError::MarkerChangedSnapshot);
            }
            let mut association_is_attached = true;
            for body in &branches {
                let record = body_records.get(body).ok_or(DocumentError::InvalidArchive(
                    "a feature branch references a missing body",
                ))?;
                let expected = if record.created_by == feature.id {
                    Some(SnapshotId::ZERO)
                } else {
                    branch_snapshots.get(body).copied().flatten()
                };
                let branch_is_attached = expected == Some(commit.input);
                association_is_attached &= branch_is_attached;
                if !branch_is_attached && feature.state.rebuild == RebuildState::Clean {
                    let expected =
                        expected.ok_or(DocumentError::BodyHasNoCommittedSnapshot(*body))?;
                    return Err(DocumentError::SnapshotChainMismatch {
                        expected,
                        actual: commit.input,
                    });
                }
            }
            if association_is_attached {
                for body in &branches {
                    branch_snapshots.insert(*body, Some(commit.output));
                }
            }
            if association_is_attached {
                for output in &feature.outputs {
                    if let FeatureOutput::Sketch {
                        sketch,
                        geometry_revision,
                    } = output
                    {
                        sketch_states.insert(*sketch, (Some(commit.output), *geometry_revision));
                    }
                }
            }
        }
        if feature.state.rebuild == RebuildState::Dirty {
            for output in &feature.outputs {
                if let FeatureOutput::Sketch {
                    sketch,
                    geometry_revision,
                } = output
                {
                    let state =
                        sketch_states
                            .get_mut(sketch)
                            .ok_or(DocumentError::InvalidArchive(
                                "a dirty sketch output precedes sketch creation",
                            ))?;
                    state.1 = *geometry_revision;
                }
            }
        }
    }
    if seen_bodies.len() != body_records.len()
        || seen_sketches.len() != sketch_records.len()
        || state.bodies.iter().any(|body| {
            seen_bodies.get(&body.id).copied() != Some(body.last_feature)
                || !positions.contains_key(&body.created_by)
        })
        || state.sketches.iter().any(|sketch| {
            seen_sketches.get(&sketch.id).copied() != Some(sketch.last_feature)
                || !positions.contains_key(&sketch.created_by)
        })
    {
        return Err(DocumentError::InvalidArchive(
            "object producer or last-feature associations are inconsistent",
        ));
    }
    for body in &state.bodies {
        validate_label(&body.label)?;
    }
    for sketch in &state.sketches {
        validate_label(&sketch.label)?;
        if let Some(consumer) = sketch.auto_hidden_by {
            let Some(feature) = state.features.iter().find(|feature| feature.id == consumer) else {
                return Err(DocumentError::InvalidArchive(
                    "a sketch auto-hide references a missing feature",
                ));
            };
            if !matches!(
                feature.kind,
                FeatureKind::Extrude | FeatureKind::Add | FeatureKind::Cut
            ) || !feature.inputs.contains(&FeatureInput::Sketch(sketch.id))
            {
                return Err(DocumentError::InvalidArchive(
                    "a sketch auto-hide is not owned by a consuming modeling feature",
                ));
            }
        }
    }
    let mut reconciled = state.clone();
    reconcile_active_object_state(&mut reconciled);
    if state.head_snapshot != reconciled.head_snapshot {
        return Err(DocumentError::InvalidArchive(
            "the head snapshot does not match the active history cursor",
        ));
    }
    if state
        .bodies
        .iter()
        .zip(&reconciled.bodies)
        .any(|(body, active)| {
            body.id != active.id || body.committed_snapshot != active.committed_snapshot
        })
    {
        return Err(DocumentError::InvalidArchive(
            "a body snapshot does not match its active history cursor",
        ));
    }
    if state
        .sketches
        .iter()
        .zip(&reconciled.sketches)
        .any(|(sketch, active)| {
            sketch.id != active.id
                || sketch.committed_snapshot != active.committed_snapshot
                || sketch.geometry_revision != active.geometry_revision
        })
    {
        return Err(DocumentError::InvalidArchive(
            "a sketch snapshot or geometry revision does not match its active history cursor",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        CURRENT_PROTOCOL_VERSION, EntityId, EntityKind, EntityRef, ExecuteRequest,
        FaceExtrusionOperation, OperationRole, PlanarFrame3, PlanarProfile2, Point2, Point3,
        PrecisionPolicy, RequestId, Vector3,
    };

    use crate::persistent::{PersistentRef, TargetedKernel};

    use super::*;

    fn snapshot(byte: u8) -> SnapshotId {
        SnapshotId::new([byte; 16])
    }

    fn digest(byte: u8) -> SemanticDigest {
        SemanticDigest::new([byte; 32])
    }

    fn association(input: u8, output: u8) -> SnapshotAssociation {
        SnapshotAssociation::new(snapshot(input), snapshot(output), digest(output))
    }

    fn cuboid_action(size_x: f64) -> ReplayAction {
        ReplayAction::Kernel(KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x,
            size_y: 3.0,
            size_z: 4.0,
        })
    }

    fn face_extrusion_command(target_face: EntityRef) -> KernelCommand {
        KernelCommand::ExtrudeFaceProfile {
            target_face,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 4.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            vertices: vec![
                Point2::new(0.25, 0.25),
                Point2::new(0.75, 0.25),
                Point2::new(0.75, 0.75),
                Point2::new(0.25, 0.75),
            ],
            distance: 1.0,
            operation: FaceExtrusionOperation::Add,
        }
    }

    fn face_sketch_payload(producer: FeatureId, body: BodyId) -> SketchPayload {
        SketchPayload::new(
            PlanarFrame3::new(
                Point3::new(0.0, 0.0, 4.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            PlanarProfile2::from_polygon(&[
                Point2::new(0.25, 0.25),
                Point2::new(0.75, 0.25),
                Point2::new(0.75, 0.75),
                Point2::new(0.25, 0.75),
            ]),
            SketchSupportRecipe::PlanarFace {
                body,
                face: PersistentRef::new(
                    producer,
                    OperationRole::new("face", Some(0)),
                    EntityKind::Face,
                ),
            },
        )
        .expect("test sketch payload should be valid")
    }

    fn origin_sketch_payload() -> SketchPayload {
        SketchPayload::new(
            PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            PlanarProfile2::from_polygon(&[
                Point2::new(-1.0, -1.0),
                Point2::new(1.0, -1.0),
                Point2::new(1.0, 1.0),
                Point2::new(-1.0, 1.0),
            ]),
            SketchSupportRecipe::Origin,
        )
        .expect("test origin sketch payload should be valid")
    }

    fn committed_base(document: &mut ModelDocument) -> (FeatureId, BodyId) {
        let result = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Base body", cuboid_action(2.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 1".to_owned(),
                    })
                    .with_commit(association(0, 1)),
            )
            .expect("base feature should append");
        (result.feature, result.created_bodies[0])
    }

    fn committed_sketch(
        document: &mut ModelDocument,
        base: FeatureId,
        body: BodyId,
    ) -> (FeatureId, SketchId) {
        let result = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch 1", ReplayAction::Marker)
                    .with_dependency(base)
                    .with_input(FeatureInput::Body(body))
                    .with_sketch_payload(face_sketch_payload(base, body))
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 1".to_owned(),
                        geometry_revision: 3,
                    })
                    .with_commit(association(1, 1)),
            )
            .expect("sketch feature should append");
        (result.feature, result.created_sketches[0])
    }

    fn append_dirty_extrude(
        document: &mut ModelDocument,
        body: BodyId,
        sketch: SketchId,
    ) -> FeatureId {
        document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Extrude 1", cuboid_action(4.0))
                    .with_input(FeatureInput::Body(body))
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_output(OutputDraft::ModifyBody(body)),
            )
            .expect("extrude should append")
            .feature
    }

    fn committed_body_step(
        document: &mut ModelDocument,
        body: BodyId,
        label: &str,
        input: u8,
        output: u8,
    ) -> FeatureId {
        document
            .append_feature(
                FeatureDraft::new(FeatureKind::Transform, label, cuboid_action(output as f64))
                    .with_input(FeatureInput::Body(body))
                    .with_output(OutputDraft::ModifyBody(body))
                    .with_commit(association(input, output)),
            )
            .expect("committed body step should append")
            .feature
    }

    #[test]
    fn history_cursor_rolls_cached_associations_backward_and_forward() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        let extrude = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Extrude 1", cuboid_action(4.0))
                    .with_input(FeatureInput::Body(body))
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_output(OutputDraft::ModifyBody(body))
                    .with_commit(association(1, 2)),
            )
            .expect("committed extrusion should append")
            .feature;
        let retained_features = document.features().to_vec();

        assert_eq!(document.history_cursor(), HistoryCursor::End);
        assert_eq!(document.history_position(), 3);
        assert_eq!(
            document.body(body).unwrap().committed_snapshot,
            Some(snapshot(2))
        );
        document
            .set_history_cursor(HistoryCursor::After(sketch_feature))
            .expect("rollback boundary should move");

        assert_eq!(document.history_position(), 2);
        assert!(document.feature_is_active(sketch_feature).unwrap());
        assert!(!document.feature_is_active(extrude).unwrap());
        assert_eq!(
            document.body(body).unwrap().committed_snapshot,
            Some(snapshot(1))
        );
        assert_eq!(
            document.sketch(sketch).unwrap().committed_snapshot,
            Some(snapshot(1))
        );
        assert_eq!(document.features(), retained_features.as_slice());
        assert_eq!(document.plan_rebuild().unwrap(), None);

        document
            .set_history_cursor(HistoryCursor::End)
            .expect("roll-forward should move");
        assert_eq!(
            document.body(body).unwrap().committed_snapshot,
            Some(snapshot(2))
        );
        assert_eq!(
            document.feature(extrude).unwrap().committed,
            Some(association(1, 2))
        );
    }

    #[test]
    fn history_rollback_and_explicit_suppression_remain_independent_states() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        let extrude = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Cut, "Cut 1", cuboid_action(1.0))
                    .with_input(FeatureInput::Body(body))
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_output(OutputDraft::ModifyBody(body))
                    .with_commit(association(1, 2)),
            )
            .expect("committed cut should append")
            .feature;
        document
            .set_feature_suppressed(extrude, true)
            .expect("cut should suppress");
        document
            .set_history_cursor(HistoryCursor::After(sketch_feature))
            .expect("cursor should roll behind cut");

        let cut = document.feature(extrude).unwrap();
        assert!(
            cut.state.suppressed,
            "rollback must not rewrite suppression"
        );
        assert!(!document.feature_is_active(extrude).unwrap());
        assert_eq!(document.plan_rebuild().unwrap(), None);

        document
            .set_history_position(3)
            .expect("cursor should roll forward");
        let plan = document
            .plan_rebuild()
            .expect("suppressed feature should plan")
            .expect("rolled-forward dirty suppression should rebuild");
        assert_eq!(plan.from, extrude);
        assert_eq!(
            plan.steps[0].disposition,
            ReplayDisposition::Skip(SkipReason::ExplicitSuppression)
        );
    }

    #[test]
    fn interleaved_body_branches_resolve_independently_at_each_history_boundary() {
        let mut document = ModelDocument::default();
        let (_, body_one) = committed_base(&mut document);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("second body should append");
        let body_two = second.created_bodies[0];
        let body_one_step = committed_body_step(&mut document, body_one, "Move body 1", 1, 2);
        let body_two_step = committed_body_step(&mut document, body_two, "Move body 2", 10, 11);

        document
            .set_history_position(2)
            .expect("cursor should move");
        assert_eq!(
            document.body(body_one).unwrap().committed_snapshot,
            Some(snapshot(1))
        );
        assert_eq!(
            document.body(body_two).unwrap().committed_snapshot,
            Some(snapshot(10))
        );
        assert_eq!(document.head_snapshot(), Some(snapshot(10)));

        document
            .set_history_cursor(HistoryCursor::After(body_one_step))
            .expect("cursor should include body one step");
        assert_eq!(
            document.body(body_one).unwrap().committed_snapshot,
            Some(snapshot(2))
        );
        assert_eq!(
            document.body(body_two).unwrap().committed_snapshot,
            Some(snapshot(10))
        );
        assert_eq!(document.head_snapshot(), Some(snapshot(2)));

        document
            .set_history_cursor(HistoryCursor::After(body_two_step))
            .expect("cursor should return to the end");
        assert_eq!(document.history_cursor(), HistoryCursor::End);
        assert_eq!(
            document.body(body_one).unwrap().committed_snapshot,
            Some(snapshot(2))
        );
        assert_eq!(
            document.body(body_two).unwrap().committed_snapshot,
            Some(snapshot(11))
        );

        document
            .set_history_position(0)
            .expect("cursor should move to start");
        assert_eq!(document.body(body_one).unwrap().committed_snapshot, None);
        assert_eq!(document.body(body_two).unwrap().committed_snapshot, None);
        assert_eq!(document.head_snapshot(), None);
    }

    #[test]
    fn rebuild_planning_stops_at_cursor_and_retains_future_branch_dirtiness() {
        let mut document = ModelDocument::default();
        let (base_one, body_one) = committed_base(&mut document);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("second body should append");
        let body_two = second.created_bodies[0];
        let (_, sketch_one) = committed_sketch(&mut document, base_one, body_one);
        let dirty_one = append_dirty_extrude(&mut document, body_one, sketch_one);
        let clean_two = committed_body_step(&mut document, body_two, "Move body 2", 10, 11);

        document
            .set_history_position(3)
            .expect("cursor should hide later features");
        assert_eq!(document.plan_rebuild().unwrap(), None);
        assert_eq!(
            document.plan_rebuild_from(dirty_one),
            Err(DocumentError::FeatureBeyondHistoryCursor(dirty_one))
        );
        assert!(!document.feature_is_active(clean_two).unwrap());

        document
            .set_history_position(5)
            .expect("cursor should roll forward");
        let plan = document
            .plan_rebuild()
            .expect("active plan should be valid")
            .expect("future dirty branch should remain dirty");
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.feature)
                .collect::<Vec<_>>(),
            vec![dirty_one]
        );
        assert_eq!(plan.branch_base(body_one), Some(Some(snapshot(1))));
        assert_eq!(plan.branch_base(body_two), None);
    }

    #[test]
    fn rebuilding_before_cursor_keeps_stale_future_cache_detached_until_roll_forward_replay() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let future = committed_body_step(&mut document, body, "Future move", 1, 2);
        document
            .set_history_position(1)
            .expect("future should roll back");
        document
            .replace_feature_action(base, cuboid_action(9.0))
            .expect("active base recipe should edit");

        let mut transaction = document
            .begin_rebuild(base)
            .expect("base should rebuild alone");
        assert_eq!(
            transaction
                .plan()
                .steps
                .iter()
                .map(|step| step.feature)
                .collect::<Vec<_>>(),
            vec![base]
        );
        transaction
            .record_success(base, association(0, 3))
            .expect("base should replay from empty snapshot");
        document
            .commit_rebuild(transaction)
            .expect("base should publish");
        assert_eq!(
            document.body(body).unwrap().committed_snapshot,
            Some(snapshot(3))
        );
        assert_eq!(
            document.feature(future).unwrap().committed,
            Some(association(1, 2))
        );
        assert_eq!(
            document.feature(future).unwrap().state.rebuild,
            RebuildState::Dirty
        );

        let encoded = serde_json::to_string(&document)
            .expect("a deferred stale association remains a valid cache entry");
        let mut restored: ModelDocument =
            serde_json::from_str(&encoded).expect("archive should load");
        restored
            .set_history_position(2)
            .expect("future should roll forward");
        assert_eq!(
            restored.body(body).unwrap().committed_snapshot,
            Some(snapshot(3)),
            "stale future cache must not replace the attached branch head"
        );

        let mut transaction = restored
            .begin_rebuild(future)
            .expect("future feature should now rebuild");
        assert_eq!(
            transaction.plan().branch_base(body),
            Some(Some(snapshot(3)))
        );
        transaction
            .record_success(future, association(3, 4))
            .expect("future result should chain from rebuilt base");
        restored
            .commit_rebuild(transaction)
            .expect("future result should publish");
        assert_eq!(
            restored.body(body).unwrap().committed_snapshot,
            Some(snapshot(4))
        );
    }

    #[test]
    fn history_cursor_moves_use_the_bounded_undo_journal() {
        let mut empty = ModelDocument::default();
        assert!(!empty.set_history_position(0).unwrap());
        assert!(
            !empty.can_undo(),
            "an empty-timeline no-op needs no checkpoint"
        );

        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let _ = committed_sketch(&mut document, base, body);
        let _ = committed_body_step(&mut document, body, "Move", 1, 2);
        document.clear_undo_history();
        document
            .set_undo_limit(2)
            .expect("small limit should be valid");

        document.set_history_position(2).unwrap();
        document.set_history_position(1).unwrap();
        document.set_history_position(0).unwrap();
        assert!(document.undo());
        assert_eq!(document.history_position(), 1);
        assert!(document.undo());
        assert_eq!(document.history_position(), 2);
        assert!(
            !document.undo(),
            "oldest cursor checkpoint should be evicted"
        );
        assert!(document.redo());
        assert_eq!(document.history_position(), 1);
        assert!(document.redo());
        assert_eq!(document.history_position(), 0);
    }

    #[test]
    fn history_cursor_round_trips_and_rejects_dangling_serialized_markers() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let _ = committed_sketch(&mut document, base, body);
        document
            .set_history_position(1)
            .expect("cursor should move");

        let encoded = serde_json::to_string(&document).expect("document should serialize");
        let restored: ModelDocument =
            serde_json::from_str(&encoded).expect("valid cursor should deserialize");
        assert_eq!(restored.history_cursor(), HistoryCursor::After(base));
        assert_eq!(restored.history_position(), 1);
        assert_eq!(
            restored.body(body).unwrap().committed_snapshot,
            Some(snapshot(1))
        );

        let mut native = document.to_native();
        native.state.history_cursor = HistoryCursor::After(FeatureId::from_allocated(99_999));
        assert_eq!(
            ModelDocument::from_native(native).expect_err("dangling cursor must be rejected"),
            DocumentError::InvalidArchive("the history cursor references a missing feature")
        );
    }

    #[test]
    fn append_requires_the_history_cursor_at_the_end_without_mutation() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let _ = committed_sketch(&mut document, base, body);
        document
            .set_history_position(1)
            .expect("cursor should roll back");
        let before = document.to_native();
        let error = document
            .append_feature(FeatureDraft::new(
                FeatureKind::Origin,
                "Future feature",
                ReplayAction::Marker,
            ))
            .expect_err("branch insertion is not yet supported");
        assert_eq!(error, DocumentError::HistoryCursorNotAtEnd);
        assert_eq!(document.to_native(), before);

        document
            .set_history_position(2)
            .expect("cursor should roll forward");
        document
            .append_feature(FeatureDraft::new(
                FeatureKind::Origin,
                "At end",
                ReplayAction::Marker,
            ))
            .expect("append should resume at timeline end");
        assert_eq!(document.history_cursor(), HistoryCursor::End);
        assert_eq!(document.history_position(), 3);
    }

    #[test]
    fn stable_ids_are_monotonic_and_never_reused_after_undo() {
        let mut document = ModelDocument::default();
        let (_, first_body) = committed_base(&mut document);
        assert!(document.undo());
        let (_, second_body) = committed_base(&mut document);

        assert_eq!(first_body.get(), 1);
        assert_eq!(second_body.get(), 2);
        assert_eq!(document.features()[0].id.get(), 2);
    }

    #[test]
    fn object_inputs_add_their_latest_producers_as_ordered_dependencies() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let node = document.feature(extrude).expect("extrude exists");

        assert_eq!(node.dependencies, vec![base, sketch_feature]);
        assert_eq!(
            node.inputs,
            vec![FeatureInput::Body(body), FeatureInput::Sketch(sketch)]
        );
    }

    #[test]
    fn body_and_sketch_visibility_are_undoable_without_making_geometry_dirty() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        assert!(document.set_body_visible(body, false).expect("body exists"));
        assert!(
            document
                .set_sketch_visible(sketch, false)
                .expect("sketch exists")
        );
        assert!(!document.body(body).expect("body exists").visible);
        assert!(!document.sketch(sketch).expect("sketch exists").visible);
        assert!(document.undo());
        assert!(!document.body(body).expect("body exists").visible);
        assert!(document.sketch(sketch).expect("sketch exists").visible);
        assert!(document.undo());
        assert!(document.body(body).expect("body exists").visible);
        assert!(
            document
                .features()
                .iter()
                .all(|feature| feature.state.rebuild == RebuildState::Clean)
        );
        assert!(document.redo());
        assert!(!document.body(body).expect("body exists").visible);
    }

    #[test]
    fn consumed_sketch_auto_hide_is_atomic_with_its_latest_feature() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let consumer = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Extrude 1", cuboid_action(4.0))
                    .with_input(FeatureInput::Body(body))
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_output(OutputDraft::ModifyBody(body))
                    .with_commit(association(1, 2)),
            )
            .expect("committed consumer should append")
            .feature;

        assert!(
            document
                .auto_hide_sketch_consumed_by(sketch, consumer)
                .expect("latest consumer owns the sketch")
        );
        assert!(!document.sketch(sketch).expect("sketch exists").visible);

        assert!(document.undo());
        assert!(document.feature(consumer).is_none());
        assert!(document.sketch(sketch).expect("sketch exists").visible);

        assert!(document.redo());
        assert!(document.feature(consumer).is_some());
        assert!(!document.sketch(sketch).expect("sketch exists").visible);
    }

    #[test]
    fn read_only_features_reject_recipe_and_suppression_changes() {
        let mut document = ModelDocument::default();
        let (base, _) = committed_base(&mut document);
        assert!(
            document
                .set_feature_read_only(base, true)
                .expect("feature exists")
        );
        assert_eq!(
            document.set_feature_suppressed(base, true),
            Err(DocumentError::ReadOnlyFeature(base))
        );
        assert_eq!(
            document.replace_feature_action(base, cuboid_action(5.0)),
            Err(DocumentError::ReadOnlyFeature(base))
        );
    }

    #[test]
    fn suppression_propagates_to_dependent_replay_steps() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        assert!(
            document
                .set_feature_suppressed(sketch_feature, true)
                .expect("sketch feature exists")
        );
        let plan = document
            .plan_rebuild_from(sketch_feature)
            .expect("plan should build");

        assert_eq!(
            plan.steps[0].disposition,
            ReplayDisposition::Skip(SkipReason::ExplicitSuppression)
        );
        assert_eq!(plan.steps[1].feature, extrude);
        assert_eq!(
            plan.steps[1].disposition,
            ReplayDisposition::Skip(SkipReason::SuppressedDependency(sketch_feature))
        );
        assert_eq!(plan.executable_count(), 0);
    }

    #[test]
    fn replacing_a_logical_sketch_is_one_undo_unit_and_dirties_prior_consumers() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        let extrude = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Extrude 1", cuboid_action(4.0))
                    .with_input(FeatureInput::Body(body))
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_output(OutputDraft::ModifyBody(body))
                    .with_commit(association(1, 2)),
            )
            .expect("committed extrude should append")
            .feature;
        let feature_count = document.features().len();
        let original_payload = document
            .sketch_payload(sketch, 3)
            .expect("original sketch payload")
            .clone();
        let edited_payload = SketchPayload::new(
            original_payload.frame,
            PlanarProfile2::from_polygon(&[
                Point2::new(0.125, 0.25),
                Point2::new(0.875, 0.25),
                Point2::new(0.875, 0.75),
                Point2::new(0.125, 0.75),
            ]),
            original_payload.support.clone(),
        )
        .expect("edited payload");

        assert!(
            document
                .replace_sketch_payload(sketch, edited_payload.clone())
                .expect("logical sketch edit should publish")
        );
        assert_eq!(document.features().len(), feature_count);
        let record = document.sketch(sketch).expect("stable sketch identity");
        assert_eq!(record.created_by, sketch_feature);
        assert_eq!(record.last_feature, sketch_feature);
        assert_eq!(record.geometry_revision, 4);
        assert_eq!(
            document.feature(sketch_feature).unwrap().state.rebuild,
            RebuildState::Dirty
        );
        assert_eq!(
            document.feature(extrude).unwrap().state.rebuild,
            RebuildState::Dirty
        );
        let plan = document
            .plan_rebuild_from(sketch_feature)
            .expect("edited branch should plan");
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.feature)
                .collect::<Vec<_>>(),
            vec![sketch_feature, extrude]
        );

        assert!(
            document.undo(),
            "one undo restores the complete sketch edit"
        );
        assert_eq!(document.sketch(sketch).unwrap().geometry_revision, 3);
        assert_eq!(document.sketch_payload(sketch, 3), Some(&original_payload));
        assert_eq!(
            document.feature(sketch_feature).unwrap().state.rebuild,
            RebuildState::Clean
        );
        assert_eq!(
            document.feature(extrude).unwrap().state.rebuild,
            RebuildState::Clean
        );
        assert!(document.redo(), "redo restores the one logical edit");
        assert_eq!(document.sketch(sketch).unwrap().geometry_revision, 4);
        assert_eq!(document.sketch_payload(sketch, 4), Some(&edited_payload));
    }

    #[test]
    fn committed_append_rejects_clean_suppressed_or_uncommitted_dependencies() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        let dependent = append_dirty_extrude(&mut document, body, sketch);
        document
            .set_feature_suppressed(sketch_feature, true)
            .expect("sketch should suppress");
        let transaction = document
            .begin_rebuild(sketch_feature)
            .expect("suppressed sketch should plan");
        document
            .commit_rebuild(transaction)
            .expect("suppression should publish");
        let before = document.to_native();

        let error = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Invalid dependent", cuboid_action(6.0))
                    .with_input(FeatureInput::Body(body))
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_output(OutputDraft::ModifyBody(body))
                    .with_commit(association(1, 2)),
            )
            .expect_err("a suppressed result cannot feed an already committed feature");

        assert_eq!(error, DocumentError::UnavailableDependency(sketch_feature));
        assert_eq!(document.to_native(), before);

        let missing_commit = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Transform,
                    "Invalid transitive dependent",
                    ReplayAction::Marker,
                )
                .with_input(FeatureInput::Feature(dependent))
                .with_commit(association(1, 1)),
            )
            .expect_err("a clean skipped result without a commit cannot feed a commit");
        assert_eq!(
            missing_commit,
            DocumentError::UnavailableDependency(dependent)
        );
        assert_eq!(document.to_native(), before);
    }

    #[test]
    fn rebuild_plan_is_deterministic_across_native_document_round_trip() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let before = document.plan_rebuild().expect("valid plan");
        let json = serde_json::to_string(&document).expect("document should serialize");
        let restored: ModelDocument =
            serde_json::from_str(&json).expect("document should deserialize");
        let after = restored.plan_rebuild().expect("valid restored plan");

        assert_eq!(before, after);
        assert_eq!(before.expect("dirty plan").from, extrude);
        assert!(
            !restored.can_undo(),
            "runtime undo is intentionally not saved"
        );
    }

    #[test]
    fn dirty_created_sketch_round_trips_with_its_intended_geometry_revision() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let created = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Sketch,
                    "Uncommitted sketch",
                    ReplayAction::Marker,
                )
                .with_input(FeatureInput::Body(body))
                .with_sketch_payload(face_sketch_payload(base, body))
                .with_output(OutputDraft::CreateSketch {
                    label: "Sketch 1".to_owned(),
                    geometry_revision: 7,
                }),
            )
            .expect("a dirty sketch intent should append");
        let sketch = created.created_sketches[0];
        let before_plan = document
            .plan_rebuild_from(created.feature)
            .expect("dirty sketch should plan");

        let json = serde_json::to_string(&document).expect("dirty sketch should serialize");
        let restored: ModelDocument =
            serde_json::from_str(&json).expect("dirty sketch should deserialize");
        let record = restored
            .sketch(sketch)
            .expect("sketch identity should persist");

        assert_eq!(record.support_body, Some(body));
        assert_eq!(record.geometry_revision, 7);
        assert_eq!(record.committed_snapshot, None);
        assert_eq!(record.created_by, created.feature);
        assert_eq!(
            restored
                .feature(created.feature)
                .expect("feature should persist")
                .state
                .rebuild,
            RebuildState::Dirty
        );
        assert_eq!(
            restored
                .plan_rebuild_from(created.feature)
                .expect("restored dirty sketch should plan"),
            before_plan
        );
        assert_eq!(
            restored
                .feature(created.feature)
                .expect("feature should persist")
                .dependencies,
            vec![base]
        );
    }

    #[test]
    fn dirty_sketch_revision_rebuilds_on_its_supporting_body_branch() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let revision = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch revision", ReplayAction::Marker)
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_sketch_payload(face_sketch_payload(base, body))
                    .with_output(OutputDraft::ModifySketch {
                        sketch,
                        geometry_revision: 8,
                    }),
            )
            .expect("dirty sketch revision should append")
            .feature;
        let mut transaction = document
            .begin_rebuild(revision)
            .expect("sketch revision should plan on its supporting body");

        assert_eq!(transaction.plan().steps[0].branches, vec![body]);
        assert_eq!(
            transaction.plan().branch_base(body),
            Some(Some(snapshot(1)))
        );
        transaction
            .record_success(revision, association(1, 1))
            .expect("marker should retain the body snapshot");
        document
            .commit_rebuild(transaction)
            .expect("sketch revision should publish");

        let record = document.sketch(sketch).expect("sketch should remain");
        assert_eq!(record.support_body, Some(body));
        assert_eq!(record.geometry_revision, 8);
        assert_eq!(record.committed_snapshot, Some(snapshot(1)));
        serde_json::from_str::<ModelDocument>(
            &serde_json::to_string(&document).expect("revision should serialize"),
        )
        .expect("rebuilt sketch revision should deserialize");
    }

    #[test]
    fn failed_rebuild_rolls_back_without_publishing_partial_results() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let before_head = document.head_snapshot();
        let before_revision = document.revision();
        let mut transaction = document
            .begin_rebuild(extrude)
            .expect("rebuild should begin");
        transaction
            .record_failure(extrude, "profile no longer resolves")
            .expect("failure should record");
        let receipt = document
            .rollback_rebuild(transaction)
            .expect("rollback should succeed");

        assert_eq!(receipt.discarded_results, 0);
        assert_eq!(receipt.retained_head, before_head);
        assert_eq!(document.revision(), before_revision);
        assert_eq!(
            document
                .feature(extrude)
                .expect("extrude exists")
                .state
                .rebuild,
            RebuildState::Dirty
        );
        assert_eq!(document.head_snapshot(), before_head);
    }

    #[test]
    fn completed_rebuild_atomically_associates_feature_body_and_head() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let mut transaction = document
            .begin_rebuild(extrude)
            .expect("rebuild should begin");
        transaction
            .record_success(extrude, association(1, 2))
            .expect("result should record");
        let receipt = document
            .commit_rebuild(transaction)
            .expect("rebuild should commit");

        assert_eq!(receipt.head_snapshot, Some(snapshot(2)));
        assert_eq!(document.head_snapshot(), Some(snapshot(2)));
        assert_eq!(
            document.feature(extrude).expect("extrude exists").committed,
            Some(association(1, 2))
        );
        assert_eq!(
            document.body(body).expect("body exists").committed_snapshot,
            Some(snapshot(2))
        );
        assert_eq!(
            document
                .feature(extrude)
                .expect("extrude exists")
                .state
                .rebuild,
            RebuildState::Clean
        );
    }

    #[test]
    fn stale_rebuild_cannot_overwrite_a_newer_user_edit() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let mut transaction = document
            .begin_rebuild(extrude)
            .expect("rebuild should begin");
        transaction
            .record_success(extrude, association(1, 2))
            .expect("result should record");
        document
            .rename_feature(extrude, "Changed while rebuilding")
            .expect("rename should succeed");

        assert!(matches!(
            document.commit_rebuild(transaction),
            Err(DocumentError::StaleRebuild { .. })
        ));
        assert_eq!(document.head_snapshot(), Some(snapshot(1)));
    }

    #[test]
    fn native_envelope_is_versioned_and_rejects_unknown_versions() {
        let mut document = ModelDocument::default();
        committed_base(&mut document);
        let native = document.to_native();
        assert_eq!(native.format(), NATIVE_DOCUMENT_FORMAT);
        assert_eq!(native.version(), CURRENT_DOCUMENT_VERSION);

        let mut value = serde_json::to_value(native).expect("archive should encode");
        value["version"] = serde_json::json!(CURRENT_DOCUMENT_VERSION + 1);
        let error = serde_json::from_value::<ModelDocument>(value)
            .expect_err("unknown version must fail closed");
        assert!(
            error
                .to_string()
                .contains("unsupported native document version")
        );
    }

    #[test]
    fn version_one_documents_migrate_in_memory_and_write_current_version() {
        let mut document = ModelDocument::default();
        committed_base(&mut document);
        let mut legacy = document.to_native();
        legacy.version = 1;

        let migrated = ModelDocument::from_native(legacy).expect("version one remains readable");
        assert_eq!(migrated.to_native().version(), CURRENT_DOCUMENT_VERSION);
    }

    #[test]
    fn tampered_forward_dependency_is_rejected_on_load() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let mut native = document.to_native();
        native.state.features[0].dependencies.push(extrude);

        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::InvalidArchive(
                "a feature dependency must precede its consumer"
            ))
        ));
    }

    #[test]
    fn undo_limit_is_bounded_and_discards_oldest_checkpoints() {
        let mut document = ModelDocument::default();
        document.set_undo_limit(2).expect("limit is valid");
        let (base, _) = committed_base(&mut document);
        document
            .rename_feature(base, "Base A")
            .expect("rename should succeed");
        document
            .rename_feature(base, "Base B")
            .expect("rename should succeed");

        assert!(document.undo());
        assert_eq!(document.feature(base).expect("base exists").label, "Base A");
        assert!(document.undo());
        assert_eq!(
            document.feature(base).expect("base exists").label,
            "Base body"
        );
        assert!(!document.undo(), "the append checkpoint was evicted");
    }

    #[test]
    fn replay_transaction_enforces_feature_and_snapshot_order() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let extrude = append_dirty_extrude(&mut document, body, sketch);
        let mut transaction = document
            .begin_rebuild(extrude)
            .expect("rebuild should begin");

        assert!(matches!(
            transaction.record_success(base, association(1, 2)),
            Err(DocumentError::RebuildOutOfOrder { .. })
        ));
        assert!(matches!(
            transaction.record_success(extrude, association(9, 2)),
            Err(DocumentError::SnapshotChainMismatch { .. })
        ));
    }

    #[test]
    fn independent_body_roots_keep_separate_snapshot_chains_and_rebuilds() {
        let mut document = ModelDocument::default();
        let (base_one, body_one) = committed_base(&mut document);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Base body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("a second body may independently consume the empty snapshot");
        let body_two = second.created_bodies[0];
        let base_two = second.feature;

        let (_, sketch_one) = committed_sketch(&mut document, base_one, body_one);
        let extrude_one = append_dirty_extrude(&mut document, body_one, sketch_one);
        let sketch_two_result = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch 2", ReplayAction::Marker)
                    .with_input(FeatureInput::Body(body_two))
                    .with_sketch_payload(face_sketch_payload(base_two, body_two))
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 2".to_owned(),
                        geometry_revision: 1,
                    })
                    .with_commit(association(10, 10)),
            )
            .expect("a clean second branch may commit while the first is dirty");
        let sketch_two = sketch_two_result.created_sketches[0];
        let extrude_two = append_dirty_extrude(&mut document, body_two, sketch_two);

        let plan_one = document
            .plan_rebuild_from(extrude_one)
            .expect("first branch should plan");
        assert_eq!(
            plan_one
                .steps
                .iter()
                .map(|step| step.feature)
                .collect::<Vec<_>>(),
            vec![extrude_one]
        );
        assert_eq!(plan_one.branch_base(body_one), Some(Some(snapshot(1))));
        assert_eq!(plan_one.branch_base(body_two), None);

        let mut transaction = document
            .begin_rebuild(extrude_one)
            .expect("first branch should rebuild");
        transaction
            .record_success(extrude_one, association(1, 2))
            .expect("first branch result should chain from body one");
        document
            .commit_rebuild(transaction)
            .expect("first branch should publish atomically");

        assert_eq!(
            document
                .body(body_one)
                .expect("body one exists")
                .committed_snapshot,
            Some(snapshot(2))
        );
        assert_eq!(
            document
                .body(body_two)
                .expect("body two exists")
                .committed_snapshot,
            Some(snapshot(10))
        );
        assert_eq!(
            document
                .feature(extrude_two)
                .expect("second extrude exists")
                .state
                .rebuild,
            RebuildState::Dirty
        );

        let plan_two = document
            .plan_rebuild()
            .expect("second plan is valid")
            .expect("second branch remains dirty");
        assert_eq!(plan_two.from, extrude_two);
        assert_eq!(plan_two.branch_base(body_two), Some(Some(snapshot(10))));
        assert_eq!(plan_two.branch_base(body_one), None);
        assert_eq!(
            document
                .feature(base_two)
                .expect("second base exists")
                .committed,
            Some(association(0, 10))
        );
    }

    #[test]
    fn editing_one_root_marks_only_its_dependency_branch_dirty() {
        let mut document = ModelDocument::default();
        let (base_one, _) = committed_base(&mut document);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Base body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("second body should append");
        document
            .replace_feature_action(base_one, cuboid_action(7.0))
            .expect("first body should edit");

        assert_eq!(
            document
                .feature(base_one)
                .expect("first base exists")
                .state
                .rebuild,
            RebuildState::Dirty
        );
        assert_eq!(
            document
                .feature(second.feature)
                .expect("second base exists")
                .state
                .rebuild,
            RebuildState::Clean
        );
        let plan = document
            .plan_rebuild()
            .expect("plan is valid")
            .expect("first root is dirty");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].feature, base_one);
        assert_eq!(plan.branch_bases.len(), 1);
        assert_eq!(plan.branch_bases[0].snapshot, None);
        assert_eq!(plan.branch_bases[0].replay_input, Some(SnapshotId::ZERO));

        let mut transaction = document
            .begin_rebuild(base_one)
            .expect("first root rebuild should begin");
        transaction
            .record_success(base_one, association(0, 3))
            .expect("root should replay from the empty snapshot");
        document
            .commit_rebuild(transaction)
            .expect("first root should publish");
        assert_eq!(
            document
                .body(second.created_bodies[0])
                .expect("second body exists")
                .committed_snapshot,
            Some(snapshot(10)),
            "rebuilding the first root must not retarget the second body"
        );
    }

    #[test]
    fn new_root_rebuild_requires_the_empty_snapshot() {
        let mut document = ModelDocument::default();
        let created = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "New root", cuboid_action(2.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 1".to_owned(),
                    }),
            )
            .expect("dirty root should append");
        let mut transaction = document
            .begin_rebuild(created.feature)
            .expect("root rebuild should begin");

        assert_eq!(
            transaction.record_success(created.feature, association(9, 1)),
            Err(DocumentError::SnapshotChainMismatch {
                expected: SnapshotId::ZERO,
                actual: snapshot(9),
            })
        );
    }

    #[test]
    fn marker_rebuild_cannot_publish_a_changed_snapshot() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, _) = committed_sketch(&mut document, base, body);
        document
            .set_feature_suppressed(sketch_feature, true)
            .expect("sketch should suppress");
        document
            .set_feature_suppressed(sketch_feature, false)
            .expect("sketch should unsuppress");
        let mut transaction = document
            .begin_rebuild(sketch_feature)
            .expect("sketch rebuild should begin");

        assert_eq!(
            transaction.record_success(sketch_feature, association(1, 2)),
            Err(DocumentError::MarkerChangedSnapshot)
        );
    }

    #[test]
    fn body_changing_recipe_must_output_its_input_body() {
        let mut document = ModelDocument::default();
        let (_, body) = committed_base(&mut document);
        let error = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Transform,
                    "Broken transform",
                    cuboid_action(3.0),
                )
                .with_input(FeatureInput::Body(body)),
            )
            .expect_err("body-changing recipe without a body output must fail");

        assert_eq!(error, DocumentError::BranchBodyMustBeOutput(body));
    }

    #[test]
    fn archive_rejects_a_body_modifier_with_a_severed_input_dependency() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        append_dirty_extrude(&mut document, body, sketch);
        let mut native = document.to_native();
        native
            .state
            .features
            .last_mut()
            .expect("extrude exists")
            .inputs
            .retain(|input| *input != FeatureInput::Body(body));

        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::InvalidArchive(
                "a body modification must declare that body as an input"
            ))
        ));
    }

    #[test]
    fn suppressed_sketch_clears_its_committed_browser_association() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (sketch_feature, sketch) = committed_sketch(&mut document, base, body);
        document
            .set_feature_suppressed(sketch_feature, true)
            .expect("sketch should suppress");
        let transaction = document
            .begin_rebuild(sketch_feature)
            .expect("suppressed branch should plan");
        assert!(transaction.is_complete(), "the plan contains only a skip");
        document
            .commit_rebuild(transaction)
            .expect("suppression should publish");

        assert_eq!(
            document
                .sketch(sketch)
                .expect("sketch record remains in Browser")
                .committed_snapshot,
            None
        );
        let encoded = serde_json::to_string(&document).expect("document should serialize");
        serde_json::from_str::<ModelDocument>(&encoded)
            .expect("suppressed sketch document should load");
    }

    #[test]
    fn suppressing_a_sketch_revision_restores_the_previous_committed_revision() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let (_, sketch) = committed_sketch(&mut document, base, body);
        let revision_feature = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch 1 edit", ReplayAction::Marker)
                    .with_input(FeatureInput::Sketch(sketch))
                    .with_sketch_payload(face_sketch_payload(base, body))
                    .with_output(OutputDraft::ModifySketch {
                        sketch,
                        geometry_revision: 4,
                    })
                    .with_commit(association(1, 1)),
            )
            .expect("sketch revision should append")
            .feature;
        assert_eq!(
            document
                .sketch(sketch)
                .expect("sketch exists")
                .geometry_revision,
            4
        );
        document
            .set_feature_suppressed(revision_feature, true)
            .expect("revision should suppress");
        let transaction = document
            .begin_rebuild(revision_feature)
            .expect("revision suppression should plan");
        document
            .commit_rebuild(transaction)
            .expect("revision suppression should publish");

        let record = document.sketch(sketch).expect("sketch remains in Browser");
        assert_eq!(record.geometry_revision, 3);
        assert_eq!(record.committed_snapshot, Some(snapshot(1)));
        let json = serde_json::to_string(&document).expect("document should serialize");
        serde_json::from_str::<ModelDocument>(&json)
            .expect("restored revision should validate on load");
    }

    #[test]
    fn rebuilding_one_branch_preserves_an_uncommitted_sketch_on_another_body() {
        let mut document = ModelDocument::default();
        let (base_one, body_one) = committed_base(&mut document);
        let (sketch_one_feature, _) = committed_sketch(&mut document, base_one, body_one);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Base body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("second body should append");
        let dirty_sketch = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch 2", ReplayAction::Marker)
                    .with_input(FeatureInput::Body(second.created_bodies[0]))
                    .with_sketch_payload(face_sketch_payload(
                        second.feature,
                        second.created_bodies[0],
                    ))
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 2".to_owned(),
                        geometry_revision: 7,
                    }),
            )
            .expect("uncommitted second sketch should append")
            .created_sketches[0];
        document
            .set_feature_suppressed(sketch_one_feature, true)
            .expect("first sketch should suppress");
        let transaction = document
            .begin_rebuild(sketch_one_feature)
            .expect("first branch suppression should plan");
        document
            .commit_rebuild(transaction)
            .expect("first branch suppression should publish");

        let untouched = document
            .sketch(dirty_sketch)
            .expect("second sketch should remain in the document");
        assert_eq!(untouched.geometry_revision, 7);
        assert_eq!(untouched.committed_snapshot, None);
        assert_eq!(
            document
                .plan_rebuild()
                .expect("remaining dirty branch should plan")
                .expect("second sketch remains dirty")
                .steps[0]
                .feature,
            document
                .sketch(dirty_sketch)
                .expect("second sketch exists")
                .created_by
        );
    }

    #[test]
    fn raw_face_target_replay_is_rejected_without_a_persistent_recipe() {
        let mut document = ModelDocument::default();
        let (base, body) = committed_base(&mut document);
        let stale = EntityRef {
            snapshot: snapshot(1),
            entity: EntityId(42),
            kind: EntityKind::Face,
        };
        let raw = face_extrusion_command(stale);
        let raw_error = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Add,
                    "Raw face edit",
                    ReplayAction::Kernel(raw.clone()),
                )
                .with_input(FeatureInput::Body(body))
                .with_output(OutputDraft::ModifyBody(body)),
            )
            .expect_err("raw face identity must not enter the document");
        assert_eq!(raw_error, DocumentError::PersistentTargetRequired);

        let target =
            PersistentRef::new(base, OperationRole::new("face", Some(0)), EntityKind::Face);
        let targeted = TargetedKernel::new(raw, target).expect("face template should pair");
        document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Add,
                    "Persistent face edit",
                    ReplayAction::TargetedKernel(targeted),
                )
                .with_input(FeatureInput::Body(body))
                .with_output(OutputDraft::ModifyBody(body)),
            )
            .expect("persistent face recipe should append");
    }

    #[test]
    fn replacing_an_action_cannot_install_a_raw_snapshot_scoped_face_target() {
        let mut document = ModelDocument::default();
        let (base, _) = committed_base(&mut document);
        let stale = EntityRef {
            snapshot: snapshot(1),
            entity: EntityId(42),
            kind: EntityKind::Face,
        };
        let before = document.to_native();

        assert_eq!(
            document
                .replace_feature_action(base, ReplayAction::Kernel(face_extrusion_command(stale))),
            Err(DocumentError::PersistentTargetRequired)
        );
        assert_eq!(document.to_native(), before);
        assert_eq!(
            document.feature(base).expect("base exists").state.rebuild,
            RebuildState::Clean
        );
        serde_json::from_str::<ModelDocument>(
            &serde_json::to_string(&document).expect("retained document should serialize"),
        )
        .expect("rejected replacement must leave a loadable document");
    }

    #[test]
    fn body_owned_sketch_cannot_modify_an_independent_body_on_append_or_load() {
        let mut document = ModelDocument::default();
        let (base_one, body_one) = committed_base(&mut document);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Base body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("second body should append");
        let body_two = second.created_bodies[0];
        let (_, sketch_one) = committed_sketch(&mut document, base_one, body_one);
        assert_eq!(
            document
                .sketch(sketch_one)
                .expect("sketch should exist")
                .support_body,
            Some(body_one)
        );
        let before = document.to_native();

        let error = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Cross-body edit", cuboid_action(6.0))
                    .with_input(FeatureInput::Body(body_two))
                    .with_input(FeatureInput::Sketch(sketch_one))
                    .with_output(OutputDraft::ModifyBody(body_two)),
            )
            .expect_err("a sketch supported by Body 1 cannot modify Body 2");
        assert_eq!(error, DocumentError::CrossBodyFeatureUnsupported);
        assert_eq!(document.to_native(), before);

        let valid = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Add, "Body 1 edit", cuboid_action(6.0))
                    .with_input(FeatureInput::Body(body_one))
                    .with_input(FeatureInput::Sketch(sketch_one))
                    .with_output(OutputDraft::ModifyBody(body_one))
                    .with_commit(association(1, 2)),
            )
            .expect("the sketch may modify its owning body")
            .feature;
        let mut native = document.to_native();
        let feature = native
            .state
            .features
            .iter_mut()
            .find(|feature| feature.id == valid)
            .expect("feature should exist");
        feature
            .inputs
            .retain(|input| *input != FeatureInput::Body(body_one));
        feature.inputs.push(FeatureInput::Body(body_two));
        feature
            .outputs
            .retain(|output| *output != FeatureOutput::Body(body_one));
        feature.outputs.push(FeatureOutput::Body(body_two));
        feature.dependencies.push(second.feature);

        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::CrossBodyFeatureUnsupported)
        ));
    }

    #[test]
    fn explicit_boolean_feature_combines_two_body_branches_without_collapsing_tool_history() {
        let mut document = ModelDocument::default();
        let (_, target) = committed_base(&mut document);
        let tool_result = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Tool body", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Tool".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("tool body");
        let tool = tool_result.created_bodies[0];
        let recipe = BooleanFeatureRecipe {
            target,
            tool,
            operation: BooleanOperation::Union,
            keep_tool: false,
        };
        let result = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Boolean,
                    "Combine",
                    ReplayAction::Boolean(recipe),
                )
                .with_input(FeatureInput::Body(target))
                .with_input(FeatureInput::Body(tool))
                .with_output(OutputDraft::ModifyBody(target))
                .with_commit(association(1, 20)),
            )
            .expect("explicit Boolean contract");

        assert_eq!(document.body(target).unwrap().last_feature, result.feature);
        assert_eq!(
            document.body(target).unwrap().committed_snapshot,
            Some(snapshot(20))
        );
        assert_eq!(
            document.body(tool).unwrap().committed_snapshot,
            Some(snapshot(10))
        );
        assert!(!document.body(tool).unwrap().visible);
        assert!(matches!(
            document.feature(result.feature).unwrap().action,
            ReplayAction::Boolean(BooleanFeatureRecipe {
                operation: BooleanOperation::Union,
                ..
            })
        ));
        let encoded = serde_json::to_string(&document).expect("serialize Boolean document");
        let decoded: ModelDocument =
            serde_json::from_str(&encoded).expect("reload Boolean document");
        assert_eq!(
            decoded.body(target).unwrap().committed_snapshot,
            Some(snapshot(20))
        );
        assert_eq!(
            decoded.body(tool).unwrap().committed_snapshot,
            Some(snapshot(10))
        );
    }

    #[test]
    fn v4_sketch_authoring_requires_a_portable_payload_without_mutation() {
        let mut document = ModelDocument::default();
        let before = document.to_native();
        let error = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker).with_output(
                    OutputDraft::CreateSketch {
                        label: "Sketch".to_owned(),
                        geometry_revision: 1,
                    },
                ),
            )
            .expect_err("new sketches must be restart-safe");

        assert_eq!(error, DocumentError::SketchPayloadRequired);
        assert_eq!(document.to_native(), before);
    }

    #[test]
    fn exact_sketch_payload_round_trips_and_resolves_by_stable_revision() {
        let mut document = ModelDocument::default();
        let payload = origin_sketch_payload();
        let appended = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "XY profile", ReplayAction::Marker)
                    .with_sketch_payload(payload.clone())
                    .with_output(OutputDraft::CreateSketch {
                        label: "XY profile".to_owned(),
                        geometry_revision: 11,
                    }),
            )
            .expect("portable origin sketch should append");
        let sketch = appended.created_sketches[0];

        assert_eq!(document.sketch_payload(sketch, 11), Some(&payload));
        assert_eq!(document.sketch_payload(sketch, 12), None);
        let json = serde_json::to_string(&document).expect("payload should serialize");
        let restored: ModelDocument =
            serde_json::from_str(&json).expect("payload should deserialize");
        assert_eq!(restored.sketch_payload(sketch, 11), Some(&payload));
    }

    #[test]
    fn v5_profile_migrates_to_exact_editable_intent_without_inventing_a_primitive() {
        let mut document = ModelDocument::default();
        let appended = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Legacy profile", ReplayAction::Marker)
                    .with_sketch_payload(origin_sketch_payload())
                    .with_output(OutputDraft::CreateSketch {
                        label: "Legacy profile".to_owned(),
                        geometry_revision: 3,
                    }),
            )
            .expect("seed sketch should append");
        let sketch = appended.created_sketches[0];
        let mut legacy = document.to_native();
        legacy.version = 5;
        legacy.state.features[0]
            .sketch_payload
            .as_mut()
            .expect("payload")
            .authoring = None;

        let migrated = ModelDocument::from_native(legacy).expect("v5 profile should migrate");
        let payload = migrated
            .sketch_payload(sketch, 3)
            .expect("migrated payload");
        let authoring = payload.authoring().expect("editable graph");
        assert!(matches!(
            authoring.operations(),
            [artificer_sketch::SketchOperationRecord {
                recipe: artificer_sketch::SketchRecipe::LegacyImportedProfile { .. },
                ..
            }]
        ));
        assert_eq!(
            authoring.active_entities().count(),
            payload.profile.curve_count()
        );
        assert_eq!(migrated.to_native().version(), CURRENT_DOCUMENT_VERSION);
    }

    #[test]
    fn v6_archive_missing_its_editable_graph_fails_closed() {
        let mut document = ModelDocument::default();
        document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Broken v6", ReplayAction::Marker)
                    .with_sketch_payload(origin_sketch_payload())
                    .with_output(OutputDraft::CreateSketch {
                        label: "Broken v6".to_owned(),
                        geometry_revision: 1,
                    }),
            )
            .expect("seed sketch should append");
        let mut native = document.to_native();
        native.state.features[0]
            .sketch_payload
            .as_mut()
            .expect("payload")
            .authoring = None;
        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::InvalidArchive(
                "a v6 sketch payload is missing its editable authoring graph"
            ))
        ));
    }

    #[test]
    fn face_sketch_support_must_match_the_feature_body_branch() {
        let mut document = ModelDocument::default();
        let (base_one, body_one) = committed_base(&mut document);
        let second = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Body 2", cuboid_action(5.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 10)),
            )
            .expect("second body should append");
        let before = document.to_native();
        let error = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Wrong support", ReplayAction::Marker)
                    .with_input(FeatureInput::Body(body_one))
                    .with_sketch_payload(face_sketch_payload(
                        second.feature,
                        second.created_bodies[0],
                    ))
                    .with_output(OutputDraft::CreateSketch {
                        label: "Wrong support".to_owned(),
                        geometry_revision: 1,
                    }),
            )
            .expect_err("support identity cannot drift to another body");

        assert_eq!(error, DocumentError::SketchSupportMismatch);
        assert_eq!(document.to_native(), before);
        assert!(document.feature(base_one).is_some());
    }

    #[test]
    fn v1_through_v3_missing_sketch_geometry_migrates_and_remains_reloadable() {
        let mut document = ModelDocument::default();
        let appended = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Legacy sketch", ReplayAction::Marker)
                    .with_sketch_payload(origin_sketch_payload())
                    .with_output(OutputDraft::CreateSketch {
                        label: "Legacy sketch".to_owned(),
                        geometry_revision: 2,
                    }),
            )
            .expect("seed sketch should append");
        let sketch = appended.created_sketches[0];
        for source_version in 1..PORTABLE_SKETCH_DOCUMENT_VERSION {
            let mut legacy = document.to_native();
            legacy.version = source_version;
            legacy.state.features[0].sketch_payload = None;
            legacy.legacy_sketch_payload_omissions.clear();

            let migrated =
                ModelDocument::from_native(legacy).expect("pre-v4 sketch omission should migrate");
            assert_eq!(migrated.sketch_payload(sketch, 2), None);
            let current = migrated.to_native();
            assert_eq!(current.version(), CURRENT_DOCUMENT_VERSION);
            assert_eq!(
                current.legacy_sketch_payload_omissions,
                vec![appended.feature]
            );
            ModelDocument::from_native(current)
                .expect("migration provenance should preserve a second reload");
        }
    }

    #[test]
    fn unmarked_v4_sketch_payload_omission_is_rejected() {
        let mut document = ModelDocument::default();
        document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker)
                    .with_sketch_payload(origin_sketch_payload())
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch".to_owned(),
                        geometry_revision: 1,
                    }),
            )
            .expect("seed sketch should append");
        let mut native = document.to_native();
        native.state.features[0].sketch_payload = None;
        native.legacy_sketch_payload_omissions.clear();

        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::InvalidArchive(
                "a v4 sketch feature is missing its portable payload"
            ))
        ));
    }

    #[test]
    fn exhausted_serialized_revision_is_rejected() {
        let mut native = ModelDocument::default().to_native();
        native.revision = u64::MAX;
        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::InvalidArchive(
                "the reserved exhausted document revision is not loadable"
            ))
        ));
    }

    fn length_parameter_spec(key: &str) -> ParameterSpec {
        ParameterSpec::new(key, key, ParameterType::Quantity(QuantityKind::Length))
            .with_display_unit(ParameterUnit::Millimeter)
    }

    fn parameterized_cuboid_action(parameter: ParameterId) -> ReplayAction {
        ReplayAction::ParameterizedKernel(
            ParameterizedKernel::independent(
                KernelCommand::MakeCuboid {
                    origin: Point3::new(0.0, 0.0, 0.0),
                    size_x: 1.0,
                    size_y: 20.0,
                    size_z: 20.0,
                },
                vec![KernelParameterBinding::new(
                    KernelScalarTarget::MakeCuboidSizeX,
                    parameter,
                )],
            )
            .expect("parameterized cuboid recipe should validate"),
        )
    }

    #[test]
    fn typed_parameters_round_trip_in_current_native_document() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                length_parameter_spec("Length"),
                ParameterBinding::literal(ParameterValue::quantity(
                    455.0,
                    ParameterUnit::Millimeter,
                )),
            )
            .expect("parameter should append");
        let encoded = serde_json::to_string(&document).expect("document should encode");
        let decoded = serde_json::from_str::<ModelDocument>(&encoded)
            .expect("parameter document should decode");

        assert_eq!(decoded.to_native().version(), CURRENT_DOCUMENT_VERSION);
        assert_eq!(
            decoded
                .parameter(length)
                .expect("length should persist")
                .spec
                .key,
            "Length"
        );
        assert_eq!(
            decoded
                .evaluate_parameters(&ParameterOverrides::default())
                .expect("length should evaluate")
                .get(length),
            Some(&ParameterValue::quantity(455.0, ParameterUnit::Millimeter))
        );
    }

    #[test]
    fn parameterized_replay_round_trips_and_resolves_from_document_values() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                length_parameter_spec("Length"),
                ParameterBinding::literal(ParameterValue::quantity(
                    455.0,
                    ParameterUnit::Millimeter,
                )),
            )
            .expect("parameter should append");
        let feature = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Parameterized extrusion",
                    parameterized_cuboid_action(length),
                )
                .with_parameter(length)
                .with_output(OutputDraft::CreateBody {
                    label: "Extrusion body".to_owned(),
                }),
            )
            .expect("matching recipe should append")
            .feature;

        let json = serde_json::to_string(&document).expect("document should encode");
        let decoded =
            serde_json::from_str::<ModelDocument>(&json).expect("document should validate");
        let evaluated = decoded
            .evaluate_parameters(&ParameterOverrides::default())
            .expect("document parameters should evaluate");
        let resolved = decoded
            .feature(feature)
            .expect("feature should persist")
            .action
            .resolve_parameters(&evaluated)
            .expect("replay should resolve");

        assert!(matches!(
            resolved,
            ReplayAction::Kernel(KernelCommand::MakeCuboid {
                size_x: 455.0,
                size_y: 20.0,
                size_z: 20.0,
                ..
            })
        ));
        assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
    }

    #[test]
    fn parameterized_append_requires_exact_inputs_and_declared_length_types() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                length_parameter_spec("Length"),
                ParameterBinding::Unresolved,
            )
            .expect("length should append");
        let count = document
            .add_parameter(
                ParameterSpec::new("Count", "Count", ParameterType::Integer),
                ParameterBinding::Unresolved,
            )
            .expect("integer should append");
        let before = document.to_native();

        let output = || OutputDraft::CreateBody {
            label: "Body".to_owned(),
        };
        assert!(matches!(
            document.append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Missing input",
                    parameterized_cuboid_action(length),
                )
                .with_output(output())
            ),
            Err(DocumentError::ParameterizedKernel(
                ParameterizedKernelError::ParameterInputMismatch
            ))
        ));
        assert!(matches!(
            document.append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Extra input",
                    parameterized_cuboid_action(length),
                )
                .with_parameter(length)
                .with_parameter(count)
                .with_output(output())
            ),
            Err(DocumentError::ParameterizedKernel(
                ParameterizedKernelError::ParameterInputMismatch
            ))
        ));
        assert!(matches!(
            document.append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Wrong type",
                    parameterized_cuboid_action(count),
                )
                .with_parameter(count)
                .with_output(output())
            ),
            Err(DocumentError::ParameterizedKernel(
                ParameterizedKernelError::ParameterTypeMismatch {
                    parameter,
                    expected: ParameterType::Quantity(QuantityKind::Length),
                    actual: ParameterType::Integer,
                }
            )) if parameter == count
        ));
        assert_eq!(document.to_native(), before, "all rejections are atomic");
    }

    #[test]
    fn parameterized_consumers_prevent_incompatible_spec_and_action_edits() {
        let mut document = ModelDocument::default();
        let first = document
            .add_parameter(length_parameter_spec("First"), ParameterBinding::Unresolved)
            .expect("first parameter should append");
        let second = document
            .add_parameter(
                length_parameter_spec("Second"),
                ParameterBinding::Unresolved,
            )
            .expect("second parameter should append");
        let feature = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Parameterized",
                    parameterized_cuboid_action(first),
                )
                .with_parameter(first)
                .with_output(OutputDraft::CreateBody {
                    label: "Body".to_owned(),
                }),
            )
            .expect("feature should append")
            .feature;

        let before_spec = document.to_native();
        assert!(matches!(
            document.replace_parameter_spec(
                first,
                ParameterSpec::new(
                    "First",
                    "First",
                    ParameterType::Quantity(QuantityKind::Angle),
                )
                .with_display_unit(ParameterUnit::Radian),
            ),
            Err(DocumentError::ParameterizedKernel(
                ParameterizedKernelError::ParameterTypeMismatch { parameter, .. }
            )) if parameter == first
        ));
        assert_eq!(document.to_native(), before_spec);

        let before_action = document.to_native();
        assert!(matches!(
            document.replace_feature_action(feature, parameterized_cuboid_action(second)),
            Err(DocumentError::ParameterizedKernel(
                ParameterizedKernelError::ParameterInputMismatch
            ))
        ));
        assert_eq!(document.to_native(), before_action);
    }

    #[test]
    fn native_load_rejects_tampered_parameterized_feature_input_agreement() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                length_parameter_spec("Length"),
                ParameterBinding::Unresolved,
            )
            .expect("parameter should append");
        document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Parameterized",
                    parameterized_cuboid_action(length),
                )
                .with_parameter(length)
                .with_output(OutputDraft::CreateBody {
                    label: "Body".to_owned(),
                }),
            )
            .expect("feature should append");
        let mut value = serde_json::to_value(&document).expect("document should encode");
        value["state"]["features"][0]["parameter_inputs"] = serde_json::json!([]);

        assert!(matches!(
            serde_json::from_value::<ModelDocument>(value),
            Err(error) if error.to_string().contains(
                "parameterized replay inputs or declared parameter types are invalid"
            )
        ));
    }

    #[test]
    fn version_two_archive_without_f1_or_f2_fields_migrates_to_current() {
        let mut value =
            serde_json::to_value(ModelDocument::default()).expect("current document should encode");
        value["version"] = serde_json::json!(2);
        value["state"]
            .as_object_mut()
            .expect("state should be an object")
            .remove("parameters");
        value["state"]
            .as_object_mut()
            .expect("state should be an object")
            .remove("component_instances");
        value["allocators"]
            .as_object_mut()
            .expect("allocators should be an object")
            .remove("next_parameter");
        value["allocators"]
            .as_object_mut()
            .expect("allocators should be an object")
            .remove("next_component_instance");

        let migrated = serde_json::from_value::<ModelDocument>(value)
            .expect("v2 archive should receive empty parameter defaults");
        assert!(migrated.parameters().is_empty());
        assert!(migrated.component_instances().is_empty());
        assert_eq!(migrated.to_native().version(), CURRENT_DOCUMENT_VERSION);
        assert_eq!(migrated.to_native().allocators.next_parameter, 1);
        assert_eq!(migrated.to_native().allocators.next_component_instance, 1);
    }

    #[test]
    fn parameter_ids_are_not_reused_after_undo() {
        let mut document = ModelDocument::default();
        let first = document
            .add_parameter(
                length_parameter_spec("FirstLength"),
                ParameterBinding::Unresolved,
            )
            .expect("first parameter should append");
        assert!(document.undo());
        assert!(document.parameter(first).is_none());
        let second = document
            .add_parameter(
                length_parameter_spec("SecondLength"),
                ParameterBinding::Unresolved,
            )
            .expect("second parameter should append");

        assert!(second.get() > first.get());
    }

    #[test]
    fn parameter_edits_dirty_only_declared_feature_consumers() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                length_parameter_spec("Length"),
                ParameterBinding::literal(ParameterValue::quantity(
                    20.0,
                    ParameterUnit::Millimeter,
                )),
            )
            .expect("parameter should append");
        let parameterized = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Parameterized", cuboid_action(2.0))
                    .with_parameter(length)
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 1".to_owned(),
                    })
                    .with_commit(association(0, 1)),
            )
            .expect("parameterized feature should append")
            .feature;
        let independent = document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Independent", cuboid_action(3.0))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 2".to_owned(),
                    })
                    .with_commit(association(0, 2)),
            )
            .expect("independent feature should append")
            .feature;

        document
            .set_parameter_binding(
                length,
                ParameterBinding::literal(ParameterValue::quantity(
                    40.0,
                    ParameterUnit::Millimeter,
                )),
            )
            .expect("binding should update");

        assert_eq!(
            document
                .feature(parameterized)
                .expect("feature exists")
                .state
                .rebuild,
            RebuildState::Dirty
        );
        assert_eq!(
            document
                .feature(independent)
                .expect("feature exists")
                .state
                .rebuild,
            RebuildState::Clean
        );
        assert_eq!(
            document.remove_parameter(length),
            Err(DocumentError::ParameterInUse {
                parameter: length,
                feature: parameterized,
            })
        );
    }

    #[test]
    fn rejected_parameter_binding_edit_is_non_mutating() {
        let mut document = ModelDocument::default();
        let length = document
            .add_parameter(
                length_parameter_spec("Length"),
                ParameterBinding::Unresolved,
            )
            .expect("parameter should append");
        let before = document.to_native();
        let error = document
            .set_parameter_binding(
                length,
                ParameterBinding::expression(ParameterExpression::literal(
                    ParameterValue::quantity(90.0, ParameterUnit::Degree),
                )),
            )
            .expect_err("angle expression cannot bind a length");

        assert!(matches!(
            error,
            DocumentError::Parameter(ParameterError::TypeMismatch { .. })
        ));
        assert_eq!(document.to_native(), before);
    }

    fn component_definition() -> ComponentDefinitionRef {
        ComponentDefinitionRef::new(
            "profiles.aluminium-2020",
            ComponentDefinitionRevision::new(1, 0, 0),
            ComponentContentDigest::from_bytes([42; 32]),
        )
        .expect("component definition should validate")
    }

    fn resolved_length(length: f64) -> EvaluatedParameters {
        EvaluatedParameters::try_from_values(BTreeMap::from([(
            ParameterId::from_allocated(1),
            ParameterValue::quantity(length, ParameterUnit::Millimeter),
        )]))
        .expect("resolved length should be canonical")
    }

    fn append_component(
        document: &mut ModelDocument,
        label: &str,
        length: f64,
        output: u8,
    ) -> AppendFeatureResult {
        let draft = ComponentInstanceDraft::new(
            label,
            component_definition(),
            resolved_length(length),
            RigidComponentPose::identity(),
        );
        document
            .append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, label, cuboid_action(length))
                    .with_component_instance(draft)
                    .with_output(OutputDraft::CreateBody {
                        label: format!("{label} body"),
                    })
                    .with_commit(association(0, output)),
            )
            .expect("component occurrence should append")
    }

    #[test]
    fn three_equal_variants_share_a_digest_but_keep_distinct_instance_ids() {
        let mut document = ModelDocument::default();
        let instances = (1..=3)
            .map(|ordinal| {
                append_component(
                    &mut document,
                    &format!("Extrusion {ordinal}"),
                    455.0,
                    ordinal,
                )
                .created_component_instance
                .expect("append should create an occurrence")
            })
            .collect::<Vec<_>>();

        assert_eq!(instances.len(), BTreeSet::from_iter(instances.iter()).len());
        let digests = instances
            .iter()
            .map(|id| {
                document
                    .component_instance(*id)
                    .expect("component should exist")
                    .binding_digest
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(digests.len(), 1, "equal variants share a cache key");
        assert_eq!(document.component_instances().len(), 3);
    }

    #[test]
    fn component_append_is_atomic_with_all_new_body_outputs() {
        let mut document = ModelDocument::default();
        let component = ComponentInstanceDraft::new(
            "Two-body bracket",
            component_definition(),
            resolved_length(100.0),
            RigidComponentPose::identity(),
        );
        let result = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Two-body bracket",
                    cuboid_action(1.0),
                )
                .with_component_instance(component)
                .with_output(OutputDraft::CreateBody {
                    label: "Bracket A".to_owned(),
                })
                .with_output(OutputDraft::CreateBody {
                    label: "Bracket B".to_owned(),
                })
                .with_commit(association(0, 1)),
            )
            .expect("component may own multiple new bodies");
        let component = document
            .component_instance(
                result
                    .created_component_instance
                    .expect("component ID should return"),
            )
            .expect("component should exist");

        assert_eq!(component.created_by, result.feature);
        assert_eq!(component.bodies, result.created_bodies);
        assert_eq!(component.bodies.len(), 2);
        let json = serde_json::to_string(&document).expect("component document should encode");
        serde_json::from_str::<ModelDocument>(&json)
            .expect("multi-body component should validate on reload");

        document
            .set_feature_suppressed(result.feature, true)
            .expect("component feature should suppress");
        assert_eq!(
            document
                .plan_rebuild_from(result.feature)
                .expect("multi-body component root should plan")
                .steps[0]
                .branches
                .len(),
            2
        );
    }

    #[test]
    fn component_pose_and_flags_are_undoable_without_dirtying_geometry() {
        let mut document = ModelDocument::default();
        let result = append_component(&mut document, "Extrusion", 300.0, 1);
        let id = result
            .created_component_instance
            .expect("component should be created");
        let pose = RigidComponentPose::new(
            ComponentTranslation::new(10.0, 20.0, 30.0).expect("translation should validate"),
            CanonicalQuaternion::new(0.0, 0.0, 0.0, -2.0).expect("quaternion should canonicalize"),
        );

        document
            .set_component_pose(id, pose)
            .expect("pose should update");
        document
            .set_component_visible(id, false)
            .expect("visibility should update");
        document
            .set_component_suppressed(id, true)
            .expect("suppression should update");
        assert_eq!(
            document
                .feature(result.feature)
                .expect("feature should exist")
                .state
                .rebuild,
            RebuildState::Clean
        );
        assert!(document.undo());
        assert!(
            !document
                .component_instance(id)
                .expect("component should remain")
                .visible
        );
        assert!(document.undo());
        assert!(
            document
                .component_instance(id)
                .expect("component should remain")
                .visible
        );
        assert_eq!(
            document
                .component_instance(id)
                .expect("component should remain")
                .pose,
            pose
        );
    }

    #[test]
    fn grounded_component_rejects_pose_edits_without_mutation() {
        let mut document = ModelDocument::default();
        let id = append_component(&mut document, "Grounded extrusion", 300.0, 1)
            .created_component_instance
            .expect("component should be created");
        document
            .set_component_grounded(id, true)
            .expect("component should ground");
        let before = document.to_native();
        let moved = RigidComponentPose::new(
            ComponentTranslation::new(1.0, 0.0, 0.0).expect("translation should validate"),
            CanonicalQuaternion::identity(),
        );

        assert_eq!(
            document.set_component_pose(id, moved),
            Err(DocumentError::GroundedComponent(id))
        );
        assert_eq!(document.to_native(), before);
    }

    #[test]
    fn component_ids_are_not_reused_after_undo() {
        let mut document = ModelDocument::default();
        let first = append_component(&mut document, "First", 100.0, 1)
            .created_component_instance
            .expect("component should be created");
        assert!(document.undo());
        let second = append_component(&mut document, "Second", 100.0, 2)
            .created_component_instance
            .expect("component should be created");

        assert!(second.get() > first.get());
        assert!(document.component_instance(first).is_none());
    }

    #[test]
    fn archive_rejects_tampered_component_binding_digest() {
        let mut document = ModelDocument::default();
        append_component(&mut document, "Extrusion", 100.0, 1);
        let mut native = document.to_native();
        native.state.component_instances[0].binding_digest =
            ParameterBindingDigest::from_bytes([0; 32]);

        assert!(matches!(
            ModelDocument::from_native(native),
            Err(DocumentError::Component(
                ComponentError::BindingDigestMismatch
            ))
        ));
    }

    #[test]
    fn invalid_component_append_leaves_document_unchanged() {
        let mut document = ModelDocument::default();
        let before = document.to_native();
        let component = ComponentInstanceDraft::new(
            "Bodyless",
            component_definition(),
            resolved_length(10.0),
            RigidComponentPose::identity(),
        );

        assert_eq!(
            document.append_feature(
                FeatureDraft::new(FeatureKind::BaseBody, "Bodyless", ReplayAction::Marker)
                    .with_component_instance(component)
            ),
            Err(DocumentError::InvalidComponentFeature)
        );
        assert_eq!(document.to_native(), before);
    }

    fn revolute_joint_kind() -> JointKind {
        JointKind::Revolute {
            origin: JointOrigin::new(1.0, 2.0, 3.0).expect("joint origin should validate"),
            axis: JointAxis::new(0.0, 0.0, 5.0).expect("joint axis should normalize"),
            limits: Some(
                RevoluteLimits::new(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2)
                    .expect("joint limits should validate"),
            ),
        }
    }

    #[test]
    fn joint_scalars_are_finite_normalized_and_bounded() {
        let axis = JointAxis::new(0.0, 3.0, 4.0).expect("finite axis should normalize");
        assert_eq!((axis.x(), axis.y(), axis.z()), (0.0, 0.6, 0.8));
        assert_eq!(JointAxis::new(0.0, 0.0, 0.0), Err(JointError::InvalidAxis));
        assert_eq!(
            JointOrigin::new(f64::INFINITY, 0.0, 0.0),
            Err(JointError::InvalidOrigin)
        );
        assert_eq!(
            RevoluteLimits::new(1.0, -1.0),
            Err(JointError::InvalidRevoluteLimits)
        );
        assert_eq!(
            RevoluteLimits::new(0.0, assembly::MAX_REVOLUTE_LIMIT_RADIANS + 1.0),
            Err(JointError::InvalidRevoluteLimits)
        );

        let value = serde_json::json!({ "x": 0.0, "y": 0.0, "z": 2.0 });
        let error = serde_json::from_value::<JointAxis>(value)
            .expect_err("serialized axes must already be canonical");
        assert!(error.to_string().contains("unit length"));
    }

    #[test]
    fn fixed_and_revolute_joints_round_trip_as_a_stable_hierarchy() {
        let mut document = ModelDocument::default();
        let root = append_component(&mut document, "Root", 100.0, 1)
            .created_component_instance
            .expect("root component should exist");
        let child = append_component(&mut document, "Child", 80.0, 2)
            .created_component_instance
            .expect("child component should exist");
        let world_joint = document
            .add_joint(JointDraft::new(
                "Root fixture",
                JointParent::World,
                root,
                JointKind::Fixed,
            ))
            .expect("world joint should append");
        let axle_joint = document
            .add_joint(JointDraft::new(
                "Axle rotation",
                JointParent::Component(root),
                child,
                revolute_joint_kind(),
            ))
            .expect("revolute joint should append");

        assert_eq!(
            document
                .joint(world_joint)
                .expect("joint should exist")
                .child,
            root
        );
        assert_eq!(
            document
                .joint_for_child(child)
                .expect("child should have one parent")
                .id,
            axle_joint
        );
        let json = serde_json::to_string(&document).expect("assembly should serialize");
        let decoded = serde_json::from_str::<ModelDocument>(&json)
            .expect("assembly should validate after reload");
        assert_eq!(decoded.joints(), document.joints());
        assert_eq!(decoded.to_native().version(), CURRENT_DOCUMENT_VERSION);
    }

    #[test]
    fn joint_graph_rejects_second_parents_and_cycles_atomically() {
        let mut document = ModelDocument::default();
        let first = append_component(&mut document, "First", 10.0, 1)
            .created_component_instance
            .expect("component should exist");
        let second = append_component(&mut document, "Second", 10.0, 2)
            .created_component_instance
            .expect("component should exist");
        let parent_joint = document
            .add_joint(JointDraft::new(
                "First to second",
                JointParent::Component(first),
                second,
                JointKind::Fixed,
            ))
            .expect("first edge should append");
        let before_cycle = document.to_native();
        assert_eq!(
            document.add_joint(JointDraft::new(
                "Cycle",
                JointParent::Component(second),
                first,
                JointKind::Fixed,
            )),
            Err(DocumentError::JointCycle(first))
        );
        assert_eq!(document.to_native(), before_cycle);

        document
            .remove_joint(parent_joint)
            .expect("edge should be removable");
        let world = document
            .add_joint(JointDraft::new(
                "World parent",
                JointParent::World,
                second,
                JointKind::Fixed,
            ))
            .expect("world edge should append");
        let before_second_parent = document.to_native();
        assert_eq!(
            document.add_joint(JointDraft::new(
                "Second parent",
                JointParent::Component(first),
                second,
                JointKind::Fixed,
            )),
            Err(DocumentError::JointChildAlreadyParented {
                child: second,
                existing: world,
            })
        );
        assert_eq!(document.to_native(), before_second_parent);
    }

    #[test]
    fn joint_mutations_are_undoable_and_do_not_change_component_pose_rules() {
        let mut document = ModelDocument::default();
        let parent = append_component(&mut document, "Parent", 10.0, 1)
            .created_component_instance
            .expect("component should exist");
        let child = append_component(&mut document, "Child", 10.0, 2)
            .created_component_instance
            .expect("component should exist");
        let joint = document
            .add_joint(JointDraft::new(
                "Fixture",
                JointParent::World,
                child,
                JointKind::Fixed,
            ))
            .expect("joint should append");
        document
            .rename_joint(joint, "Pivot")
            .expect("joint should rename");
        document
            .set_joint_parent(joint, JointParent::Component(parent))
            .expect("joint should reparent");
        document
            .set_joint_kind(joint, revolute_joint_kind())
            .expect("joint recipe should update");
        document
            .set_joint_enabled(joint, false)
            .expect("joint should disable");

        let moved = RigidComponentPose::new(
            ComponentTranslation::new(9.0, 8.0, 7.0).expect("translation should validate"),
            CanonicalQuaternion::identity(),
        );
        document
            .set_component_pose(child, moved)
            .expect("the existing direct-pose API remains valid with a joint");
        assert_eq!(
            document
                .component_instance(child)
                .expect("component should exist")
                .pose,
            moved
        );
        assert!(document.undo(), "pose edit should undo first");
        assert!(document.undo(), "enabled edit should be undoable");
        assert!(document.joint(joint).expect("joint should exist").enabled);
        assert!(document.redo());
        assert!(!document.joint(joint).expect("joint should exist").enabled);
    }

    #[test]
    fn joint_ids_are_not_reused_after_undo() {
        let mut document = ModelDocument::default();
        let component = append_component(&mut document, "Child", 10.0, 1)
            .created_component_instance
            .expect("component should exist");
        let first = document
            .add_joint(JointDraft::new(
                "First",
                JointParent::World,
                component,
                JointKind::Fixed,
            ))
            .expect("joint should append");
        assert!(document.undo());
        let second = document
            .add_joint(JointDraft::new(
                "Second",
                JointParent::World,
                component,
                JointKind::Fixed,
            ))
            .expect("replacement joint should append");
        assert!(second.get() > first.get());
        assert!(document.joint(first).is_none());
    }

    #[test]
    fn version_four_without_joint_fields_migrates_to_version_five() {
        let mut document = ModelDocument::default();
        append_component(&mut document, "Legacy component", 10.0, 1);
        let mut value = serde_json::to_value(document.to_native()).expect("archive should encode");
        value["version"] = serde_json::json!(4);
        value["state"]
            .as_object_mut()
            .expect("state should be an object")
            .remove("joints");
        value["allocators"]
            .as_object_mut()
            .expect("allocators should be an object")
            .remove("next_joint");

        let migrated = serde_json::from_value::<ModelDocument>(value)
            .expect("v4 archive should receive empty assembly defaults");
        assert!(migrated.joints().is_empty());
        assert_eq!(migrated.to_native().allocators.next_joint, 1);
        assert_eq!(migrated.to_native().version(), CURRENT_DOCUMENT_VERSION);
    }

    #[test]
    fn archive_rejects_cyclic_and_over_capacity_joint_graphs() {
        let mut document = ModelDocument::default();
        let first = append_component(&mut document, "First", 10.0, 1)
            .created_component_instance
            .expect("component should exist");
        let second = append_component(&mut document, "Second", 10.0, 2)
            .created_component_instance
            .expect("component should exist");
        document
            .add_joint(JointDraft::new(
                "First to second",
                JointParent::Component(first),
                second,
                JointKind::Fixed,
            ))
            .expect("joint should append");
        let mut cyclic = document.to_native();
        let injected_id = JointId::from_allocated(cyclic.allocators.next_joint);
        cyclic.allocators.next_joint += 1;
        cyclic.state.joints.push(JointRecord {
            id: injected_id,
            name: "Second to first".to_owned(),
            parent: JointParent::Component(second),
            child: first,
            kind: JointKind::Fixed,
            enabled: false,
        });
        assert!(matches!(
            ModelDocument::from_native(cyclic),
            Err(DocumentError::InvalidArchive(
                "the assembly joint hierarchy contains a cycle"
            ))
        ));

        let mut over_capacity = document.to_native();
        let retained = over_capacity.state.joints[0].clone();
        over_capacity.state.joints = vec![retained; assembly::MAX_JOINTS + 1];
        assert!(matches!(
            ModelDocument::from_native(over_capacity),
            Err(DocumentError::CapacityExceeded {
                resource: "assembly joints",
                limit: assembly::MAX_JOINTS,
            })
        ));
    }

    #[test]
    fn model_document_stays_protocol_data_only() {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("model-test"),
            expected_snapshot: snapshot(0),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 2.0,
                size_y: 3.0,
                size_z: 4.0,
            },
        };
        let action = ReplayAction::Kernel(request.command);
        assert!(matches!(action, ReplayAction::Kernel(_)));
    }

    #[test]
    fn rebuild_plan_groups_independent_branches_into_stable_parallel_waves() {
        let first_body = BodyId::from_allocated(1);
        let second_body = BodyId::from_allocated(2);
        let step = |timeline_index, feature, body| RebuildStep {
            timeline_index,
            feature: FeatureId::from_allocated(feature),
            branches: vec![body],
            action: ReplayAction::Marker,
            disposition: ReplayDisposition::Execute,
        };
        let plan = RebuildPlan {
            base_revision: 7,
            from: FeatureId::from_allocated(1),
            branch_bases: Vec::new(),
            steps: vec![
                step(0, 1, first_body),
                step(1, 2, second_body),
                step(2, 3, first_body),
                step(3, 4, second_body),
            ],
        };

        let feature_waves = plan
            .parallel_waves()
            .into_iter()
            .map(|wave| {
                wave.into_iter()
                    .map(|step| step.feature.get())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(feature_waves, vec![vec![1, 2], vec![3, 4]]);
    }
}

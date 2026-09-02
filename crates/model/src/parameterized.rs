//! Typed late binding from document parameters into kernel command recipes.
//!
//! A parameterized recipe retains a fully serializable kernel-command
//! template and a small, explicit list of scalar field bindings.  It does not
//! execute expressions: callers first obtain canonical [`EvaluatedParameters`]
//! from the document parameter table, then resolve this recipe into an ordinary
//! replay action.  This keeps the saved document declarative and prevents UI or
//! catalog code from performing untyped command mutation.

use std::collections::BTreeSet;

use artificer_protocol::KernelCommand;
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

use crate::persistent::TargetedKernel;
use crate::{
    EvaluatedParameters, ParameterId, ParameterTable, ParameterType, ParameterUnit, ParameterValue,
    QuantityKind, ReplayAction,
};

/// Maximum number of scalar command fields one replay recipe may late-bind.
///
/// The initial commands expose at most three bindable fields, but the slightly
/// larger ceiling leaves room for future typed fields without allowing an
/// unbounded document payload.
pub const MAX_KERNEL_PARAMETER_BINDINGS: usize = 16;

/// One supported scalar field inside a [`KernelCommand`] template.
///
/// Targets are deliberately command-specific.  A generic string/path would be
/// easier to extend but could silently bind the wrong field after a schema
/// change. Every target currently consumes a canonical length in millimetres.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelScalarTarget {
    MakeCuboidSizeX,
    MakeCuboidSizeY,
    MakeCuboidSizeZ,
    ExtrudePlanarProfileDistance,
    ExtrudeFaceProfileDistance,
    ExtrudeFacePlanarProfileDistance,
}

impl KernelScalarTarget {
    /// Declared parameter type accepted by this command field.
    #[must_use]
    pub const fn parameter_type(self) -> ParameterType {
        match self {
            Self::MakeCuboidSizeX
            | Self::MakeCuboidSizeY
            | Self::MakeCuboidSizeZ
            | Self::ExtrudePlanarProfileDistance
            | Self::ExtrudeFaceProfileDistance
            | Self::ExtrudeFacePlanarProfileDistance => {
                ParameterType::Quantity(QuantityKind::Length)
            }
        }
    }
}

/// A stable document parameter bound to one typed command field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelParameterBinding {
    pub target: KernelScalarTarget,
    pub parameter: ParameterId,
}

impl KernelParameterBinding {
    #[must_use]
    pub const fn new(target: KernelScalarTarget, parameter: ParameterId) -> Self {
        Self { target, parameter }
    }
}

/// Snapshot relationship retained by a parameterized command template.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "template", rename_all = "snake_case")]
pub enum KernelReplayTemplate {
    /// Snapshot-independent command, such as a root body extrusion.
    Independent(KernelCommand),
    /// Command carrying a document-owned persistent entity target.
    PersistentTarget(TargetedKernel),
}

impl KernelReplayTemplate {
    /// Returns the non-authoritative scalar command template.
    #[must_use]
    pub const fn command(&self) -> &KernelCommand {
        match self {
            Self::Independent(command) => command,
            Self::PersistentTarget(targeted) => targeted.command_template(),
        }
    }
}

/// A bounded, typed parameter-to-command binding recipe.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParameterizedKernel {
    template: KernelReplayTemplate,
    bindings: Vec<KernelParameterBinding>,
}

#[derive(Deserialize)]
struct RawParameterizedKernel {
    template: KernelReplayTemplate,
    #[serde(default)]
    bindings: Vec<KernelParameterBinding>,
}

impl<'de> Deserialize<'de> for ParameterizedKernel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawParameterizedKernel::deserialize(deserializer)?;
        Self::from_template(raw.template, raw.bindings).map_err(de::Error::custom)
    }
}

impl ParameterizedKernel {
    /// Creates a snapshot-independent parameterized command recipe.
    pub fn independent(
        command_template: KernelCommand,
        bindings: Vec<KernelParameterBinding>,
    ) -> Result<Self, ParameterizedKernelError> {
        Self::from_template(
            KernelReplayTemplate::Independent(command_template),
            bindings,
        )
    }

    /// Creates a parameterized command with an authoritative persistent target.
    pub fn persistent_target(
        command_template: TargetedKernel,
        bindings: Vec<KernelParameterBinding>,
    ) -> Result<Self, ParameterizedKernelError> {
        Self::from_template(
            KernelReplayTemplate::PersistentTarget(command_template),
            bindings,
        )
    }

    fn from_template(
        template: KernelReplayTemplate,
        bindings: Vec<KernelParameterBinding>,
    ) -> Result<Self, ParameterizedKernelError> {
        let recipe = Self { template, bindings };
        recipe.validate()?;
        Ok(recipe)
    }

    /// Snapshot relationship and command template retained by this recipe.
    #[must_use]
    pub const fn template(&self) -> &KernelReplayTemplate {
        &self.template
    }

    /// Ordered scalar bindings retained by this recipe.
    #[must_use]
    pub fn bindings(&self) -> &[KernelParameterBinding] {
        &self.bindings
    }

    /// Unique parameters referenced by the scalar bindings.
    #[must_use]
    pub fn referenced_parameters(&self) -> BTreeSet<ParameterId> {
        self.bindings
            .iter()
            .map(|binding| binding.parameter)
            .collect()
    }

    /// Validates structure independently of a particular document table.
    pub fn validate(&self) -> Result<(), ParameterizedKernelError> {
        if self.bindings.is_empty() {
            return Err(ParameterizedKernelError::BindingsRequired);
        }
        if self.bindings.len() > MAX_KERNEL_PARAMETER_BINDINGS {
            return Err(ParameterizedKernelError::BindingLimitExceeded {
                limit: MAX_KERNEL_PARAMETER_BINDINGS,
            });
        }

        match &self.template {
            KernelReplayTemplate::Independent(command) => {
                if is_entity_targeting_command(command) {
                    return Err(ParameterizedKernelError::PersistentTargetRequired);
                }
            }
            KernelReplayTemplate::PersistentTarget(targeted) => targeted
                .validate()
                .map_err(|_| ParameterizedKernelError::InvalidPersistentTarget)?,
        }

        validate_supported_command(self.template.command())?;
        let mut targets = BTreeSet::new();
        for binding in &self.bindings {
            if binding.parameter.get() == 0 {
                return Err(ParameterizedKernelError::InvalidParameterId);
            }
            if !targets.insert(binding.target) {
                return Err(ParameterizedKernelError::DuplicateTarget(binding.target));
            }
            if !target_matches_command(binding.target, self.template.command()) {
                return Err(ParameterizedKernelError::TargetCommandMismatch(
                    binding.target,
                ));
            }
        }
        Ok(())
    }

    /// Validates stable input agreement and declared types against a document.
    pub fn validate_parameter_inputs(
        &self,
        parameter_inputs: &[ParameterId],
        table: &ParameterTable,
    ) -> Result<(), ParameterizedKernelError> {
        self.validate()?;
        let declared = parameter_inputs.iter().copied().collect::<BTreeSet<_>>();
        if declared.len() != parameter_inputs.len() {
            return Err(ParameterizedKernelError::DuplicateParameterInput);
        }
        let referenced = self.referenced_parameters();
        if declared != referenced {
            return Err(ParameterizedKernelError::ParameterInputMismatch);
        }
        for binding in &self.bindings {
            let record =
                table
                    .get(binding.parameter)
                    .ok_or(ParameterizedKernelError::UnknownParameter(
                        binding.parameter,
                    ))?;
            let expected = binding.target.parameter_type();
            let actual = record.spec.value_type;
            if actual != expected {
                return Err(ParameterizedKernelError::ParameterTypeMismatch {
                    parameter: binding.parameter,
                    expected,
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Resolves canonical evaluated values into an ordinary replay action.
    ///
    /// The returned action is never parameterized and can enter the existing
    /// kernel replay path directly.
    pub fn resolve(
        &self,
        parameters: &EvaluatedParameters,
    ) -> Result<ReplayAction, ParameterizedKernelError> {
        self.validate()?;
        let mut command = self.template.command().clone();
        for binding in &self.bindings {
            let value = parameters.get(binding.parameter).ok_or(
                ParameterizedKernelError::MissingEvaluatedParameter(binding.parameter),
            )?;
            let magnitude = canonical_length(binding.parameter, value)?;
            assign_scalar(&mut command, binding.target, magnitude)?;
        }
        validate_supported_command(&command)?;
        match &self.template {
            KernelReplayTemplate::Independent(_) => Ok(ReplayAction::Kernel(command)),
            KernelReplayTemplate::PersistentTarget(targeted) => {
                let rebound = TargetedKernel::new(command, targeted.target().clone())
                    .map_err(|_| ParameterizedKernelError::InvalidPersistentTarget)?;
                Ok(ReplayAction::TargetedKernel(rebound))
            }
        }
    }
}

/// Rejection produced while authoring, loading, or resolving a parameterized
/// command recipe.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ParameterizedKernelError {
    #[error("a parameterized kernel recipe requires at least one scalar binding")]
    BindingsRequired,
    #[error("a parameterized kernel recipe exceeds the binding limit of {limit}")]
    BindingLimitExceeded { limit: usize },
    #[error("parameter bindings must use non-zero stable parameter IDs")]
    InvalidParameterId,
    #[error("command field {0:?} is bound more than once")]
    DuplicateTarget(KernelScalarTarget),
    #[error("feature parameter inputs contain a duplicate stable ID")]
    DuplicateParameterInput,
    #[error("feature parameter inputs must exactly match the recipe parameter references")]
    ParameterInputMismatch,
    #[error("unknown parameter {0}")]
    UnknownParameter(ParameterId),
    #[error("parameter {parameter} has type {actual:?}; {expected:?} is required")]
    ParameterTypeMismatch {
        parameter: ParameterId,
        expected: ParameterType,
        actual: ParameterType,
    },
    #[error("evaluated parameter assignment is missing {0}")]
    MissingEvaluatedParameter(ParameterId),
    #[error("evaluated parameter {parameter} has type {actual:?}; a length is required")]
    ResolvedTypeMismatch {
        parameter: ParameterId,
        actual: ParameterType,
    },
    #[error("evaluated length parameter {0} must be finite and strictly positive")]
    InvalidResolvedLength(ParameterId),
    #[error("command field {0:?} is incompatible with the retained command template")]
    TargetCommandMismatch(KernelScalarTarget),
    #[error("the retained kernel command is not supported by typed scalar binding")]
    UnsupportedCommand,
    #[error("entity-targeting command templates require a persistent target recipe")]
    PersistentTargetRequired,
    #[error("the retained persistent target and command template are incompatible")]
    InvalidPersistentTarget,
    #[error("every bindable kernel length in the retained template must be finite and positive")]
    InvalidTemplateLength,
}

fn canonical_length(
    parameter: ParameterId,
    value: &ParameterValue,
) -> Result<f64, ParameterizedKernelError> {
    let ParameterValue::Quantity { value } = value else {
        return Err(ParameterizedKernelError::ResolvedTypeMismatch {
            parameter,
            actual: value.value_type(),
        });
    };
    if value.unit.quantity_kind() != QuantityKind::Length {
        return Err(ParameterizedKernelError::ResolvedTypeMismatch {
            parameter,
            actual: ParameterType::Quantity(value.unit.quantity_kind()),
        });
    }
    // EvaluatedParameters canonicalizes all lengths to millimetres. Checking
    // the unit here protects this boundary if that invariant changes later.
    if value.unit != ParameterUnit::Millimeter
        || !value.magnitude.is_finite()
        || value.magnitude <= 0.0
    {
        return Err(ParameterizedKernelError::InvalidResolvedLength(parameter));
    }
    Ok(value.magnitude)
}

fn validate_supported_command(command: &KernelCommand) -> Result<(), ParameterizedKernelError> {
    let lengths = match command {
        KernelCommand::MakeCuboid {
            size_x,
            size_y,
            size_z,
            ..
        } => [Some(*size_x), Some(*size_y), Some(*size_z)],
        KernelCommand::ExtrudePlanarProfile { distance, .. }
        | KernelCommand::ExtrudeFaceProfile { distance, .. }
        | KernelCommand::ExtrudeFacePlanarProfile { distance, .. } => [Some(*distance), None, None],
        KernelCommand::TransformSnapshot { .. }
        | KernelCommand::ExtrudePolygon { .. }
        | KernelCommand::PushPullFace { .. }
        | KernelCommand::MakeRevolvedAnnulus { .. }
        // A revolve carries no length parameter of its own: its dimensions
        // live in the profile and the axis, which parameterization reaches
        // through the sketch rather than through the command.
        | KernelCommand::RevolvePlanarProfile { .. }
        // A drafted loft's distance and offset drive each other through the
        // draft angle; binding one alone would silently change the angle.
        | KernelCommand::LoftPlanarProfileOffset { .. }
        | KernelCommand::DrillHole { .. }
        | KernelCommand::AddRib { .. }
        | KernelCommand::MirrorSnapshot { .. }
        | KernelCommand::LinearPatternSnapshot { .. }
        | KernelCommand::FinishEdge { .. }
        | KernelCommand::FinishEdges { .. } => {
            return Err(ParameterizedKernelError::UnsupportedCommand);
        }
    };
    if lengths
        .into_iter()
        .flatten()
        .any(|value| !value.is_finite() || value <= 0.0)
    {
        return Err(ParameterizedKernelError::InvalidTemplateLength);
    }
    Ok(())
}

const fn is_entity_targeting_command(command: &KernelCommand) -> bool {
    matches!(
        command,
        KernelCommand::ExtrudeFaceProfile { .. }
            | KernelCommand::ExtrudeFacePlanarProfile { .. }
            | KernelCommand::PushPullFace { .. }
            | KernelCommand::FinishEdge { .. }
            | KernelCommand::FinishEdges { .. }
    )
}

const fn target_matches_command(target: KernelScalarTarget, command: &KernelCommand) -> bool {
    matches!(
        (target, command),
        (
            KernelScalarTarget::MakeCuboidSizeX
                | KernelScalarTarget::MakeCuboidSizeY
                | KernelScalarTarget::MakeCuboidSizeZ,
            KernelCommand::MakeCuboid { .. }
        ) | (
            KernelScalarTarget::ExtrudePlanarProfileDistance,
            KernelCommand::ExtrudePlanarProfile { .. }
        ) | (
            KernelScalarTarget::ExtrudeFaceProfileDistance,
            KernelCommand::ExtrudeFaceProfile { .. }
        ) | (
            KernelScalarTarget::ExtrudeFacePlanarProfileDistance,
            KernelCommand::ExtrudeFacePlanarProfile { .. }
        )
    )
}

fn assign_scalar(
    command: &mut KernelCommand,
    target: KernelScalarTarget,
    value: f64,
) -> Result<(), ParameterizedKernelError> {
    match (target, command) {
        (KernelScalarTarget::MakeCuboidSizeX, KernelCommand::MakeCuboid { size_x, .. }) => {
            *size_x = value;
        }
        (KernelScalarTarget::MakeCuboidSizeY, KernelCommand::MakeCuboid { size_y, .. }) => {
            *size_y = value;
        }
        (KernelScalarTarget::MakeCuboidSizeZ, KernelCommand::MakeCuboid { size_z, .. }) => {
            *size_z = value;
        }
        (
            KernelScalarTarget::ExtrudePlanarProfileDistance,
            KernelCommand::ExtrudePlanarProfile { distance, .. },
        )
        | (
            KernelScalarTarget::ExtrudeFaceProfileDistance,
            KernelCommand::ExtrudeFaceProfile { distance, .. },
        )
        | (
            KernelScalarTarget::ExtrudeFacePlanarProfileDistance,
            KernelCommand::ExtrudeFacePlanarProfile { distance, .. },
        ) => {
            *distance = value;
        }
        (target, _) => return Err(ParameterizedKernelError::TargetCommandMismatch(target)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use artificer_protocol::{
        EntityId, EntityKind, EntityRef, FaceExtrusionOperation, OperationRole, PlanarFrame3,
        PlanarProfile2, Point2, Point3, SnapshotId, Vector3,
    };
    use serde_json::Value;

    use crate::persistent::PersistentRef;
    use crate::{FeatureId, ParameterBinding, ParameterMetadata, ParameterRecord, ParameterSpec};

    use super::*;

    fn parameter(value: u64) -> ParameterId {
        ParameterId::from_allocated(value)
    }

    fn length_table(ids: &[ParameterId]) -> ParameterTable {
        ParameterTable::try_from_records(
            ids.iter()
                .enumerate()
                .map(|(index, id)| ParameterRecord {
                    id: *id,
                    spec: ParameterSpec::new(
                        format!("Length{}", index + 1),
                        format!("Length {}", index + 1),
                        ParameterType::Quantity(QuantityKind::Length),
                    ),
                    binding: ParameterBinding::Unresolved,
                })
                .collect(),
        )
        .expect("length table should validate")
    }

    fn values(entries: &[(ParameterId, f64, ParameterUnit)]) -> EvaluatedParameters {
        EvaluatedParameters::try_from_values(
            entries
                .iter()
                .map(|(id, magnitude, unit)| (*id, ParameterValue::quantity(*magnitude, *unit)))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("evaluated values should canonicalize")
    }

    fn frame() -> PlanarFrame3 {
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        )
    }

    fn profile() -> PlanarProfile2 {
        PlanarProfile2::from_polygon(&[
            Point2::new(0.0, 0.0),
            Point2::new(2.0, 0.0),
            Point2::new(2.0, 2.0),
            Point2::new(0.0, 2.0),
        ])
    }

    fn stale_face() -> EntityRef {
        EntityRef {
            snapshot: SnapshotId::new([9; 16]),
            entity: EntityId(4),
            kind: EntityKind::Face,
        }
    }

    fn persistent_face(command: KernelCommand) -> TargetedKernel {
        TargetedKernel::new(
            command,
            PersistentRef::new(
                FeatureId::from_allocated(7),
                OperationRole::new("face", Some(1)),
                EntityKind::Face,
            ),
        )
        .expect("targeted command should validate")
    }

    #[test]
    fn cuboid_fields_resolve_canonical_lengths_and_may_share_one_parameter() {
        let width = parameter(1);
        let height = parameter(2);
        let recipe = ParameterizedKernel::independent(
            KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 1.0,
                size_y: 1.0,
                size_z: 1.0,
            },
            vec![
                KernelParameterBinding::new(KernelScalarTarget::MakeCuboidSizeX, width),
                KernelParameterBinding::new(KernelScalarTarget::MakeCuboidSizeY, width),
                KernelParameterBinding::new(KernelScalarTarget::MakeCuboidSizeZ, height),
            ],
        )
        .expect("recipe should validate");

        assert_eq!(
            recipe.referenced_parameters(),
            BTreeSet::from([width, height])
        );
        recipe
            .validate_parameter_inputs(&[height, width], &length_table(&[width, height]))
            .expect("feature input order is not semantic");
        let resolved = recipe
            .resolve(&values(&[
                (width, 2.0, ParameterUnit::Inch),
                (height, 3.0, ParameterUnit::Centimeter),
            ]))
            .expect("canonical values should resolve");

        let ReplayAction::Kernel(KernelCommand::MakeCuboid {
            size_x,
            size_y,
            size_z,
            ..
        }) = resolved
        else {
            panic!("resolution should produce a concrete cuboid");
        };
        assert_eq!((size_x, size_y, size_z), (50.8, 50.8, 30.0));
    }

    #[test]
    fn planar_profile_distance_resolves_without_changing_exact_profile() {
        let depth = parameter(1);
        let original_profile = profile();
        let recipe = ParameterizedKernel::independent(
            KernelCommand::ExtrudePlanarProfile {
                frame: frame(),
                profile: original_profile.clone(),
                distance: 1.0,
            },
            vec![KernelParameterBinding::new(
                KernelScalarTarget::ExtrudePlanarProfileDistance,
                depth,
            )],
        )
        .expect("recipe should validate");

        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile {
            profile, distance, ..
        }) = recipe
            .resolve(&values(&[(depth, 455.0, ParameterUnit::Millimeter)]))
            .expect("distance should resolve")
        else {
            panic!("resolution should produce a concrete profile extrusion");
        };
        assert_eq!(profile, original_profile);
        assert_eq!(distance, 455.0);
    }

    #[test]
    fn both_persistent_face_profile_distances_resolve_and_retain_target_recipe() {
        let depth = parameter(1);
        let linear = ParameterizedKernel::persistent_target(
            persistent_face(KernelCommand::ExtrudeFaceProfile {
                target_face: stale_face(),
                frame: frame(),
                vertices: vec![
                    Point2::new(0.0, 0.0),
                    Point2::new(1.0, 0.0),
                    Point2::new(1.0, 1.0),
                ],
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            }),
            vec![KernelParameterBinding::new(
                KernelScalarTarget::ExtrudeFaceProfileDistance,
                depth,
            )],
        )
        .expect("linear face recipe should validate");
        let analytic = ParameterizedKernel::persistent_target(
            persistent_face(KernelCommand::ExtrudeFacePlanarProfile {
                target_face: stale_face(),
                frame: frame(),
                profile: profile(),
                distance: 1.0,
                operation: FaceExtrusionOperation::Cut,
            }),
            vec![KernelParameterBinding::new(
                KernelScalarTarget::ExtrudeFacePlanarProfileDistance,
                depth,
            )],
        )
        .expect("analytic face recipe should validate");

        for recipe in [linear, analytic] {
            let ReplayAction::TargetedKernel(resolved) = recipe
                .resolve(&values(&[(depth, 12.5, ParameterUnit::Millimeter)]))
                .expect("face depth should resolve")
            else {
                panic!("face resolution must preserve persistent targeting");
            };
            assert_eq!(resolved.target().producer, FeatureId::from_allocated(7));
            match resolved.command_template() {
                KernelCommand::ExtrudeFaceProfile { distance, .. }
                | KernelCommand::ExtrudeFacePlanarProfile { distance, .. } => {
                    assert_eq!(*distance, 12.5);
                }
                _ => panic!("unexpected resolved command"),
            }
        }
    }

    #[test]
    fn recipe_serde_round_trip_is_stable_and_validated() {
        let length = parameter(1);
        let recipe = ParameterizedKernel::independent(
            KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 1.0,
                size_y: 2.0,
                size_z: 3.0,
            },
            vec![KernelParameterBinding::new(
                KernelScalarTarget::MakeCuboidSizeX,
                length,
            )],
        )
        .expect("recipe should validate");
        let json = serde_json::to_string(&recipe).expect("recipe should encode");
        let decoded: ParameterizedKernel =
            serde_json::from_str(&json).expect("recipe should decode");
        assert_eq!(decoded, recipe);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
    }

    #[test]
    fn deserialization_rejects_zero_ids_duplicates_mismatches_and_excess_bindings() {
        let length = parameter(1);
        let recipe = ParameterizedKernel::independent(
            KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 1.0,
                size_y: 2.0,
                size_z: 3.0,
            },
            vec![KernelParameterBinding::new(
                KernelScalarTarget::MakeCuboidSizeX,
                length,
            )],
        )
        .expect("recipe should validate");
        let base = serde_json::to_value(recipe).expect("recipe should encode");

        let mut zero = base.clone();
        zero["bindings"][0]["parameter"] = Value::from(0);
        assert!(serde_json::from_value::<ParameterizedKernel>(zero).is_err());

        let mut duplicate = base.clone();
        duplicate["bindings"] = Value::Array(vec![
            duplicate["bindings"][0].clone(),
            duplicate["bindings"][0].clone(),
        ]);
        assert!(serde_json::from_value::<ParameterizedKernel>(duplicate).is_err());

        let mut mismatch = base.clone();
        mismatch["bindings"][0]["target"] = Value::from("extrude_planar_profile_distance");
        assert!(serde_json::from_value::<ParameterizedKernel>(mismatch).is_err());

        let mut excessive = base;
        excessive["bindings"] = Value::Array(
            (0..=MAX_KERNEL_PARAMETER_BINDINGS)
                .map(|index| {
                    let mut binding = excessive["bindings"][0].clone();
                    binding["parameter"] = Value::from((index + 1) as u64);
                    binding
                })
                .collect(),
        );
        assert!(serde_json::from_value::<ParameterizedKernel>(excessive).is_err());
    }

    #[test]
    fn authoring_rejects_wrong_scope_target_command_and_template_ranges() {
        let length = parameter(1);
        let binding = vec![KernelParameterBinding::new(
            KernelScalarTarget::MakeCuboidSizeX,
            length,
        )];
        assert_eq!(
            ParameterizedKernel::independent(
                KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: stale_face(),
                    frame: frame(),
                    profile: profile(),
                    distance: 1.0,
                    operation: FaceExtrusionOperation::Add,
                },
                vec![KernelParameterBinding::new(
                    KernelScalarTarget::ExtrudeFacePlanarProfileDistance,
                    length,
                )],
            ),
            Err(ParameterizedKernelError::PersistentTargetRequired)
        );
        assert_eq!(
            ParameterizedKernel::independent(
                KernelCommand::MakeCuboid {
                    origin: Point3::new(0.0, 0.0, 0.0),
                    size_x: 0.0,
                    size_y: 1.0,
                    size_z: 1.0,
                },
                binding,
            ),
            Err(ParameterizedKernelError::InvalidTemplateLength)
        );
        assert_eq!(
            ParameterizedKernel::independent(
                KernelCommand::ExtrudePlanarProfile {
                    frame: frame(),
                    profile: profile(),
                    distance: 1.0,
                },
                vec![KernelParameterBinding::new(
                    KernelScalarTarget::MakeCuboidSizeX,
                    length,
                )],
            ),
            Err(ParameterizedKernelError::TargetCommandMismatch(
                KernelScalarTarget::MakeCuboidSizeX
            ))
        );
    }

    #[test]
    fn input_and_resolution_validation_rejects_missing_extra_wrong_type_and_range() {
        let length = parameter(1);
        let recipe = ParameterizedKernel::independent(
            KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 1.0,
                size_y: 1.0,
                size_z: 1.0,
            },
            vec![KernelParameterBinding::new(
                KernelScalarTarget::MakeCuboidSizeX,
                length,
            )],
        )
        .expect("recipe should validate");
        let table = length_table(&[length]);

        assert_eq!(
            recipe.validate_parameter_inputs(&[], &table),
            Err(ParameterizedKernelError::ParameterInputMismatch)
        );
        assert_eq!(
            recipe.validate_parameter_inputs(&[length, length], &table),
            Err(ParameterizedKernelError::DuplicateParameterInput)
        );
        assert_eq!(
            recipe.resolve(&EvaluatedParameters::default()),
            Err(ParameterizedKernelError::MissingEvaluatedParameter(length))
        );
        assert_eq!(
            recipe.resolve(&values(&[(length, -1.0, ParameterUnit::Millimeter)])),
            Err(ParameterizedKernelError::InvalidResolvedLength(length))
        );

        let wrong_type = ParameterTable::try_from_records(vec![ParameterRecord {
            id: length,
            spec: ParameterSpec {
                key: "Count".to_owned(),
                label: "Count".to_owned(),
                value_type: ParameterType::Integer,
                display_unit: None,
                metadata: ParameterMetadata::default(),
            },
            binding: ParameterBinding::Unresolved,
        }])
        .expect("integer table should validate");
        assert!(matches!(
            recipe.validate_parameter_inputs(&[length], &wrong_type),
            Err(ParameterizedKernelError::ParameterTypeMismatch {
                parameter,
                expected: ParameterType::Quantity(QuantityKind::Length),
                actual: ParameterType::Integer,
            }) if parameter == length
        ));
    }
}

//! Adapter between the presentation-only Part Library intent and the native
//! catalog/model/kernel data boundaries.
//!
//! The UI owns staging and confirmation. This module owns the first built-in
//! definition, seals its self-contained current-version `ModelDocument` recipe into an
//! immutable catalog package, and late-binds the required `length` value into
//! an exact kernel command. It performs no kernel execution and no workspace
//! mutation.

use std::error::Error;
use std::fmt;

use artificer_catalog::{
    CatalogError, CatalogStore, ContentDigest, DisplayUnit, EmbeddedDocument,
    ParameterId as CatalogParameterId, ParameterSpec as CatalogParameterSpec, PartDefinition,
    PartDefinitionId, PartKind, PartMetadata, PartPackage, PartRevision, RealQuantity, RealRules,
};
use artificer_model::{
    CURRENT_DOCUMENT_VERSION, DocumentError, EvaluatedParameters, FeatureDraft, FeatureKind,
    KernelParameterBinding, KernelReplayTemplate, KernelScalarTarget, ModelDocument, OutputDraft,
    ParameterBinding, ParameterBindingDigest, ParameterError, ParameterExposure, ParameterMetadata,
    ParameterOverrides, ParameterSpec, ParameterType, ParameterUnit, ParameterValue,
    ParameterizedKernel, ParameterizedKernelError, QuantityKind, ReplayAction,
};
use artificer_protocol::{KernelCommand, PlanarFrame3, PlanarProfile2, Point2, Point3, Vector3};

use crate::part_library::{
    ALUMINIUM_EXTRUSION_20X20_KEY, ALUMINIUM_EXTRUSION_20X20_NAME,
    ALUMINIUM_EXTRUSION_20X20_REVISION, LENGTH_PARAMETER_KEY, PartInsertionIntent,
};

const EXTRUSION_WIDTH_MM: f64 = 20.0;
const EXTRUSION_HEIGHT_MM: f64 = 20.0;
const MIN_LENGTH_MM: f64 = 0.001;
const MAX_LENGTH_MM: f64 = 100_000.0;
// This value makes the authored feature independently inspectable. It is not a
// user-facing default: the model binding and catalog parameter remain required.
const AUTHOR_VALIDATION_SAMPLE_LENGTH_MM: f64 = 1.0;
const NATIVE_DOCUMENT_MEDIA_TYPE: &str = "application/vnd.artificer.native+json";

/// Typed failure produced before an insertion can reach kernel execution.
#[derive(Debug)]
pub enum LibraryCatalogError {
    Catalog(CatalogError),
    Document(DocumentError),
    Parameter(ParameterError),
    ParameterizedKernel(ParameterizedKernelError),
    Json(serde_json::Error),
    DefinitionKeyMismatch {
        expected: &'static str,
        actual: String,
    },
    DefinitionRevisionMismatch {
        expected: u32,
        actual: u32,
    },
    DefinitionDigestMismatch {
        expected: String,
        actual: String,
    },
    DefinitionNameMismatch {
        expected: &'static str,
        actual: String,
    },
    MissingParameter(&'static str),
    DuplicateParameter(String),
    UnexpectedParameter(String),
    InvalidLength(String),
    PackageContract(String),
    RecipeContract(String),
}

impl fmt::Display for LibraryCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => write!(formatter, "catalog package error: {error}"),
            Self::Document(error) => write!(formatter, "model document error: {error}"),
            Self::Parameter(error) => write!(formatter, "parameter resolution error: {error}"),
            Self::ParameterizedKernel(error) => {
                write!(formatter, "parameterized recipe error: {error}")
            }
            Self::Json(error) => write!(formatter, "native document JSON error: {error}"),
            Self::DefinitionKeyMismatch { expected, actual } => write!(
                formatter,
                "insertion targets definition `{actual}`; expected `{expected}`"
            ),
            Self::DefinitionRevisionMismatch { expected, actual } => write!(
                formatter,
                "insertion targets definition revision {actual}; expected {expected}"
            ),
            Self::DefinitionDigestMismatch { expected, actual } => write!(
                formatter,
                "insertion targets package digest `{actual}`; expected `{expected}`"
            ),
            Self::DefinitionNameMismatch { expected, actual } => write!(
                formatter,
                "insertion names definition `{actual}`; expected `{expected}`"
            ),
            Self::MissingParameter(parameter) => {
                write!(formatter, "required parameter `{parameter}` is missing")
            }
            Self::DuplicateParameter(parameter) => {
                write!(
                    formatter,
                    "parameter `{parameter}` was supplied more than once"
                )
            }
            Self::UnexpectedParameter(parameter) => {
                write!(formatter, "unexpected parameter `{parameter}` was supplied")
            }
            Self::InvalidLength(reason) => write!(formatter, "invalid length: {reason}"),
            Self::PackageContract(reason) => {
                write!(formatter, "built-in package contract mismatch: {reason}")
            }
            Self::RecipeContract(reason) => {
                write!(formatter, "built-in model recipe mismatch: {reason}")
            }
        }
    }
}

impl Error for LibraryCatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Catalog(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Parameter(error) => Some(error),
            Self::ParameterizedKernel(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::DefinitionKeyMismatch { .. }
            | Self::DefinitionRevisionMismatch { .. }
            | Self::DefinitionDigestMismatch { .. }
            | Self::DefinitionNameMismatch { .. }
            | Self::MissingParameter(_)
            | Self::DuplicateParameter(_)
            | Self::UnexpectedParameter(_)
            | Self::InvalidLength(_)
            | Self::PackageContract(_)
            | Self::RecipeContract(_) => None,
        }
    }
}

impl From<CatalogError> for LibraryCatalogError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<DocumentError> for LibraryCatalogError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

impl From<ParameterError> for LibraryCatalogError {
    fn from(error: ParameterError) -> Self {
        Self::Parameter(error)
    }
}

impl From<ParameterizedKernelError> for LibraryCatalogError {
    fn from(error: ParameterizedKernelError) -> Self {
        Self::ParameterizedKernel(error)
    }
}

impl From<serde_json::Error> for LibraryCatalogError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Reproducibility evidence carried with a resolved concrete part variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedVariantEvidence {
    definition_digest: ContentDigest,
    binding_digest: ParameterBindingDigest,
    document_version: u32,
}

impl ResolvedVariantEvidence {
    #[must_use]
    pub const fn definition_digest(self) -> ContentDigest {
        self.definition_digest
    }

    #[must_use]
    pub const fn binding_digest(self) -> ParameterBindingDigest {
        self.binding_digest
    }

    #[must_use]
    pub const fn document_version(self) -> u32 {
        self.document_version
    }
}

/// Exact, side-effect-free output ready for a later component insertion gate.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedLibraryPart {
    staging_id: u64,
    evidence: ResolvedVariantEvidence,
    evaluated_parameters: EvaluatedParameters,
    command: KernelCommand,
}

impl ResolvedLibraryPart {
    #[must_use]
    pub const fn staging_id(&self) -> u64 {
        self.staging_id
    }

    #[must_use]
    pub const fn evidence(&self) -> ResolvedVariantEvidence {
        self.evidence
    }

    /// Canonical values used to bind the recipe and key component geometry.
    #[must_use]
    pub const fn evaluated_parameters(&self) -> &EvaluatedParameters {
        &self.evaluated_parameters
    }

    #[must_use]
    pub const fn command(&self) -> &KernelCommand {
        &self.command
    }
}

/// Builds and seals the first built-in parametric library definition.
///
/// Repeated calls produce byte-identical packages and the same content digest.
pub fn builtin_aluminium_extrusion_package() -> Result<PartPackage, LibraryCatalogError> {
    let document = builtin_model_recipe()?;
    let native_json = serde_json::to_vec(&document)?;
    let embedded = EmbeddedDocument::from_json(
        NATIVE_DOCUMENT_MEDIA_TYPE,
        CURRENT_DOCUMENT_VERSION,
        native_json,
    )?;

    let metadata = PartMetadata::new(ALUMINIUM_EXTRUSION_20X20_NAME)?
        .with_description(
            "Native parametric 20 × 20 mm aluminium extrusion with a required length.",
        )?
        .with_category("Structural / Aluminium Extrusion")?
        .with_material("Aluminium")?
        .with_part_number("OPEN-2020")?
        .with_tags(["aluminium", "extrusion", "metric", "parametric"])?;
    let length = CatalogParameterSpec::real(
        CatalogParameterId::parse(LENGTH_PARAMETER_KEY)?,
        "Length",
        0,
        RealQuantity::Length,
        DisplayUnit::Millimetre,
        None,
        RealRules::new(Some(MIN_LENGTH_MM), Some(MAX_LENGTH_MM), Some(0.001))?,
    )?
    .with_description("Overall extrusion length in millimetres.")?
    .with_group("Dimensions")?;
    let definition = PartDefinition::parametric(
        PartDefinitionId::parse(ALUMINIUM_EXTRUSION_20X20_KEY)?,
        builtin_part_revision(),
        metadata,
        vec![length],
        embedded,
    )?;
    let package = PartPackage::seal(definition)?;
    verify_builtin_package_contract(&package)?;
    Ok(package)
}

/// Resolves one confirmed UI intent against the immutable built-in package.
pub fn resolve_builtin_insertion(
    intent: &PartInsertionIntent,
) -> Result<ResolvedLibraryPart, LibraryCatalogError> {
    let package = builtin_aluminium_extrusion_package()?;
    resolve_insertion(&package, intent)
}

/// Resolves one UI intent through the app-owned persistent catalog store.
pub fn resolve_store_insertion(
    store: &CatalogStore,
    intent: &PartInsertionIntent,
) -> Result<ResolvedLibraryPart, LibraryCatalogError> {
    verify_intent_identity(intent)?;
    let definition = PartDefinitionId::parse(&intent.definition_key)?;
    let revision = PartRevision::new(intent.definition_revision, 0, 0);
    let package = store.resolve(&definition, revision)?;
    resolve_insertion(&package, intent)
}

/// Resolves one UI intent against an explicitly supplied immutable package.
///
/// Keeping this function package-driven lets a later `CatalogStore::resolve`
/// result use the same validation path as the built-in card.
pub fn resolve_insertion(
    package: &PartPackage,
    intent: &PartInsertionIntent,
) -> Result<ResolvedLibraryPart, LibraryCatalogError> {
    verify_intent_identity(intent)?;
    verify_builtin_package_contract(package)?;
    let expected_digest = package.content_digest().to_hex();
    if intent.definition_digest != expected_digest {
        return Err(LibraryCatalogError::DefinitionDigestMismatch {
            expected: expected_digest,
            actual: intent.definition_digest.clone(),
        });
    }
    let length_mm = intent_length(intent)?;
    let document = decode_model_recipe(package)?;
    let parameter = document
        .parameters()
        .get_by_key(LENGTH_PARAMETER_KEY)
        .ok_or(LibraryCatalogError::MissingParameter(LENGTH_PARAMETER_KEY))?;
    let mut overrides = ParameterOverrides::default();
    overrides.set(
        parameter.id,
        ParameterValue::quantity(length_mm, ParameterUnit::Millimeter),
    )?;
    let evaluated = document.evaluate_parameters(&overrides)?;
    let resolved_length = evaluated
        .get(parameter.id)
        .ok_or(LibraryCatalogError::MissingParameter(LENGTH_PARAMETER_KEY))?;
    let ParameterValue::Quantity { value } = resolved_length else {
        return Err(LibraryCatalogError::RecipeContract(
            "the length parameter did not evaluate to a quantity".into(),
        ));
    };
    if value.unit != ParameterUnit::Millimeter {
        return Err(LibraryCatalogError::RecipeContract(
            "the length parameter did not canonicalize to millimetres".into(),
        ));
    }
    validate_length(value.magnitude)?;
    let command = bind_recipe_command(&document, parameter.id, &evaluated)?;
    let binding_digest = evaluated.binding_digest();
    Ok(ResolvedLibraryPart {
        staging_id: intent.staging_id,
        evidence: ResolvedVariantEvidence {
            definition_digest: package.content_digest(),
            binding_digest,
            document_version: document.to_native().version(),
        },
        evaluated_parameters: evaluated,
        command,
    })
}

/// Checks that a package still agrees with the presentation card constants and
/// with its required native parameter boundary.
pub fn verify_builtin_package_contract(package: &PartPackage) -> Result<(), LibraryCatalogError> {
    package.verify()?;
    let definition = package.definition();
    if definition.id().as_str() != ALUMINIUM_EXTRUSION_20X20_KEY {
        return Err(LibraryCatalogError::PackageContract(format!(
            "definition key `{}` does not match `{ALUMINIUM_EXTRUSION_20X20_KEY}`",
            definition.id()
        )));
    }
    if definition.revision() != builtin_part_revision() {
        return Err(LibraryCatalogError::PackageContract(format!(
            "revision {} does not match {}.0.0",
            definition.revision(),
            ALUMINIUM_EXTRUSION_20X20_REVISION
        )));
    }
    if definition.kind() != PartKind::Parametric {
        return Err(LibraryCatalogError::PackageContract(
            "the definition is not parametric".into(),
        ));
    }
    if definition.metadata().name() != ALUMINIUM_EXTRUSION_20X20_NAME {
        return Err(LibraryCatalogError::PackageContract(format!(
            "name `{}` does not match the library card",
            definition.metadata().name()
        )));
    }
    let [length] = definition.parameters() else {
        return Err(LibraryCatalogError::PackageContract(
            "the definition must expose exactly one parameter".into(),
        ));
    };
    if length.id().as_str() != LENGTH_PARAMETER_KEY || !length.requires_input() {
        return Err(LibraryCatalogError::PackageContract(
            "`length` must be the sole required parameter with no default".into(),
        ));
    }
    let document = decode_model_recipe(package)?;
    verify_model_recipe(&document)
}

fn builtin_part_revision() -> PartRevision {
    PartRevision::new(ALUMINIUM_EXTRUSION_20X20_REVISION, 0, 0)
}

fn builtin_model_recipe() -> Result<ModelDocument, LibraryCatalogError> {
    let mut document = ModelDocument::default();
    let metadata = ParameterMetadata {
        default_value: None,
        minimum: Some(ParameterValue::quantity(
            MIN_LENGTH_MM,
            ParameterUnit::Millimeter,
        )),
        maximum: Some(ParameterValue::quantity(
            MAX_LENGTH_MM,
            ParameterUnit::Millimeter,
        )),
        step: Some(ParameterValue::quantity(0.001, ParameterUnit::Millimeter)),
        choices: Vec::new(),
        exposure: ParameterExposure::UserInput,
        description: Some("Overall extrusion length in millimetres.".into()),
        group: Some("Dimensions".into()),
        order: 0,
    };
    let spec = ParameterSpec::new(
        LENGTH_PARAMETER_KEY,
        "Length",
        ParameterType::Quantity(QuantityKind::Length),
    )
    .with_display_unit(ParameterUnit::Millimeter)
    .with_metadata(metadata);
    let length = document.add_parameter(spec, ParameterBinding::Unresolved)?;
    let recipe = ParameterizedKernel::independent(
        extrusion_command(AUTHOR_VALIDATION_SAMPLE_LENGTH_MM),
        vec![KernelParameterBinding::new(
            KernelScalarTarget::ExtrudePlanarProfileDistance,
            length,
        )],
    )?;
    document.append_feature(
        FeatureDraft::new(
            FeatureKind::Extrude,
            ALUMINIUM_EXTRUSION_20X20_NAME,
            ReplayAction::ParameterizedKernel(recipe),
        )
        .with_parameter(length)
        .with_output(OutputDraft::CreateBody {
            label: "Extrusion Body".into(),
        }),
    )?;
    verify_model_recipe(&document)?;
    Ok(document)
}

fn decode_model_recipe(package: &PartPackage) -> Result<ModelDocument, LibraryCatalogError> {
    let embedded = package.definition().document();
    if embedded.media_type() != NATIVE_DOCUMENT_MEDIA_TYPE {
        return Err(LibraryCatalogError::PackageContract(format!(
            "embedded media type `{}` is not `{NATIVE_DOCUMENT_MEDIA_TYPE}`",
            embedded.media_type()
        )));
    }
    if embedded.schema_version() != CURRENT_DOCUMENT_VERSION {
        return Err(LibraryCatalogError::PackageContract(format!(
            "embedded schema {} is not ModelDocument v{CURRENT_DOCUMENT_VERSION}",
            embedded.schema_version()
        )));
    }
    let document: ModelDocument = serde_json::from_str(embedded.canonical_json())?;
    if document.to_native().version() != CURRENT_DOCUMENT_VERSION {
        return Err(LibraryCatalogError::PackageContract(
            "decoded native document is not the current schema".into(),
        ));
    }
    Ok(document)
}

fn verify_model_recipe(document: &ModelDocument) -> Result<(), LibraryCatalogError> {
    let records = document.parameters().records();
    let [length] = records else {
        return Err(LibraryCatalogError::RecipeContract(
            "the document must define exactly one parameter".into(),
        ));
    };
    if length.spec.key != LENGTH_PARAMETER_KEY
        || length.spec.value_type != ParameterType::Quantity(QuantityKind::Length)
        || length.spec.display_unit != Some(ParameterUnit::Millimeter)
        || length.spec.metadata.exposure != ParameterExposure::UserInput
        || length.spec.metadata.default_value.is_some()
        || !length.is_required_input()
    {
        return Err(LibraryCatalogError::RecipeContract(
            "`length` must be a required, default-free millimetre input".into(),
        ));
    }
    let [feature] = document.features() else {
        return Err(LibraryCatalogError::RecipeContract(
            "the document must contain exactly one extrusion feature".into(),
        ));
    };
    if feature.kind != FeatureKind::Extrude
        || feature.parameter_inputs.as_slice() != [length.id]
        || feature.outputs.len() != 1
    {
        return Err(LibraryCatalogError::RecipeContract(
            "the extrusion feature does not consume only `length` and create one body".into(),
        ));
    }
    let ReplayAction::ParameterizedKernel(recipe) = &feature.action else {
        return Err(LibraryCatalogError::RecipeContract(
            "the extrusion feature is not a parameterized native kernel recipe".into(),
        ));
    };
    recipe.validate_parameter_inputs(&feature.parameter_inputs, document.parameters())?;
    if recipe.bindings()
        != [KernelParameterBinding::new(
            KernelScalarTarget::ExtrudePlanarProfileDistance,
            length.id,
        )]
    {
        return Err(LibraryCatalogError::RecipeContract(
            "the extrusion distance is not bound exclusively to `length`".into(),
        ));
    }
    let KernelReplayTemplate::Independent(command) = recipe.template() else {
        return Err(LibraryCatalogError::RecipeContract(
            "the library root extrusion must be snapshot-independent".into(),
        ));
    };
    verify_extrusion_shape(command)
}

fn bind_recipe_command(
    document: &ModelDocument,
    length_parameter: artificer_model::ParameterId,
    evaluated: &EvaluatedParameters,
) -> Result<KernelCommand, LibraryCatalogError> {
    let feature = document
        .features()
        .iter()
        .find(|feature| feature.parameter_inputs == [length_parameter])
        .ok_or_else(|| {
            LibraryCatalogError::RecipeContract(
                "no extrusion feature consumes the `length` parameter".into(),
            )
        })?;
    match feature.action.resolve_parameters(evaluated)? {
        ReplayAction::Kernel(command) => Ok(command),
        ReplayAction::Marker
        | ReplayAction::TargetedKernel(_)
        | ReplayAction::ParameterizedKernel(_)
        | ReplayAction::SketchRegionExtrusion(_)
        | ReplayAction::Boolean(_) => Err(LibraryCatalogError::RecipeContract(
            "the parameterized root recipe did not resolve to an independent kernel command".into(),
        )),
    }
}

fn verify_intent_identity(intent: &PartInsertionIntent) -> Result<(), LibraryCatalogError> {
    if intent.definition_key != ALUMINIUM_EXTRUSION_20X20_KEY {
        return Err(LibraryCatalogError::DefinitionKeyMismatch {
            expected: ALUMINIUM_EXTRUSION_20X20_KEY,
            actual: intent.definition_key.clone(),
        });
    }
    if intent.definition_revision != ALUMINIUM_EXTRUSION_20X20_REVISION {
        return Err(LibraryCatalogError::DefinitionRevisionMismatch {
            expected: ALUMINIUM_EXTRUSION_20X20_REVISION,
            actual: intent.definition_revision,
        });
    }
    if intent.display_name != ALUMINIUM_EXTRUSION_20X20_NAME {
        return Err(LibraryCatalogError::DefinitionNameMismatch {
            expected: ALUMINIUM_EXTRUSION_20X20_NAME,
            actual: intent.display_name.clone(),
        });
    }
    Ok(())
}

fn intent_length(intent: &PartInsertionIntent) -> Result<f64, LibraryCatalogError> {
    let mut length = None;
    for assignment in &intent.parameters {
        if assignment.key != LENGTH_PARAMETER_KEY {
            return Err(LibraryCatalogError::UnexpectedParameter(
                assignment.key.clone(),
            ));
        }
        if length.replace(assignment.value_mm).is_some() {
            return Err(LibraryCatalogError::DuplicateParameter(
                assignment.key.clone(),
            ));
        }
    }
    let value = length.ok_or(LibraryCatalogError::MissingParameter(LENGTH_PARAMETER_KEY))?;
    validate_length(value)?;
    Ok(value)
}

fn validate_length(value: f64) -> Result<(), LibraryCatalogError> {
    if !value.is_finite() {
        return Err(LibraryCatalogError::InvalidLength(
            "value must be finite".into(),
        ));
    }
    if !(MIN_LENGTH_MM..=MAX_LENGTH_MM).contains(&value) {
        return Err(LibraryCatalogError::InvalidLength(format!(
            "value must be between {MIN_LENGTH_MM} mm and {MAX_LENGTH_MM} mm"
        )));
    }
    Ok(())
}

fn extrusion_command(distance: f64) -> KernelCommand {
    KernelCommand::ExtrudePlanarProfile {
        frame: PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        profile: PlanarProfile2::from_polygon(&[
            Point2::new(0.0, 0.0),
            Point2::new(EXTRUSION_WIDTH_MM, 0.0),
            Point2::new(EXTRUSION_WIDTH_MM, EXTRUSION_HEIGHT_MM),
            Point2::new(0.0, EXTRUSION_HEIGHT_MM),
        ]),
        distance,
    }
}

fn verify_extrusion_shape(command: &KernelCommand) -> Result<(), LibraryCatalogError> {
    let expected = extrusion_command(AUTHOR_VALIDATION_SAMPLE_LENGTH_MM);
    let (
        KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance,
        },
        KernelCommand::ExtrudePlanarProfile {
            frame: expected_frame,
            profile: expected_profile,
            distance: expected_distance,
        },
    ) = (command, expected)
    else {
        return Err(LibraryCatalogError::RecipeContract(
            "expected an ExtrudePlanarProfile command".into(),
        ));
    };
    if *frame != expected_frame || *profile != expected_profile || *distance != expected_distance {
        return Err(LibraryCatalogError::RecipeContract(
            "recipe is not the exact 20 × 20 mm native validation extrusion".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::part_library::{ParameterValueSource, PartParameterAssignment};

    use super::*;

    fn intent(length_mm: f64) -> PartInsertionIntent {
        let definition_digest = builtin_aluminium_extrusion_package()
            .expect("built-in package")
            .content_digest()
            .to_hex();
        PartInsertionIntent {
            staging_id: 7,
            definition_key: ALUMINIUM_EXTRUSION_20X20_KEY.into(),
            definition_revision: ALUMINIUM_EXTRUSION_20X20_REVISION,
            definition_digest,
            display_name: ALUMINIUM_EXTRUSION_20X20_NAME.into(),
            parameters: vec![PartParameterAssignment {
                key: LENGTH_PARAMETER_KEY.into(),
                display_name: "Length".into(),
                value_mm: length_mm,
                source: ParameterValueSource::Entered,
            }],
        }
    }

    #[test]
    fn built_in_package_is_deterministic_required_and_self_contained() {
        let first = builtin_aluminium_extrusion_package().unwrap();
        let second = builtin_aluminium_extrusion_package().unwrap();
        assert_eq!(first.content_digest(), second.content_digest());
        assert_eq!(
            first.to_json_bytes().unwrap(),
            second.to_json_bytes().unwrap()
        );
        assert_eq!(
            first.definition().id().as_str(),
            ALUMINIUM_EXTRUSION_20X20_KEY
        );
        assert_eq!(first.definition().revision(), PartRevision::new(1, 0, 0));
        assert_eq!(first.definition().parameters().len(), 1);
        assert!(first.definition().parameters()[0].requires_input());

        let document = decode_model_recipe(&first).unwrap();
        assert_eq!(document.to_native().version(), CURRENT_DOCUMENT_VERSION);
        assert!(
            document
                .parameters()
                .get_by_key(LENGTH_PARAMETER_KEY)
                .unwrap()
                .is_required_input()
        );
        verify_model_recipe(&document).unwrap();
    }

    #[test]
    fn resolution_binds_exact_20_by_20_profile_and_length() {
        let resolved = resolve_builtin_insertion(&intent(455.0)).unwrap();
        assert_eq!(resolved.staging_id(), 7);
        assert_eq!(
            resolved.evidence().document_version(),
            CURRENT_DOCUMENT_VERSION
        );
        assert_eq!(
            resolved.evaluated_parameters().binding_digest(),
            resolved.evidence().binding_digest()
        );
        let KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance,
        } = resolved.command()
        else {
            panic!("expected native planar-profile extrusion");
        };
        assert_eq!(*distance, 455.0);
        assert_eq!(
            *frame,
            PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0)
            )
        );
        assert_eq!(
            *profile,
            PlanarProfile2::from_polygon(&[
                Point2::new(0.0, 0.0),
                Point2::new(20.0, 0.0),
                Point2::new(20.0, 20.0),
                Point2::new(0.0, 20.0),
            ])
        );
    }

    #[test]
    fn equal_assignments_share_binding_evidence_but_keep_staging_identity() {
        let mut first_intent = intent(310.0);
        first_intent.staging_id = 10;
        let mut second_intent = intent(310.0);
        second_intent.staging_id = 11;
        let first = resolve_builtin_insertion(&first_intent).unwrap();
        let second = resolve_builtin_insertion(&second_intent).unwrap();

        assert_ne!(first.staging_id(), second.staging_id());
        assert_eq!(first.evidence(), second.evidence());
        assert_eq!(first.command(), second.command());
        assert_eq!(
            first.evidence().binding_digest().to_string(),
            second.evidence().binding_digest().to_string()
        );
        assert_eq!(first.evidence().binding_digest().to_string().len(), 64);
    }

    #[test]
    fn different_lengths_keep_definition_digest_and_change_binding_digest() {
        let short = resolve_builtin_insertion(&intent(310.0)).unwrap();
        let long = resolve_builtin_insertion(&intent(455.0)).unwrap();
        assert_eq!(
            short.evidence().definition_digest(),
            long.evidence().definition_digest()
        );
        assert_ne!(
            short.evidence().binding_digest(),
            long.evidence().binding_digest()
        );
        assert_ne!(short.command(), long.command());
    }

    #[test]
    fn intent_identity_and_parameter_errors_fail_closed() {
        let mut wrong_key = intent(10.0);
        wrong_key.definition_key = "another-part".into();
        assert!(matches!(
            resolve_builtin_insertion(&wrong_key),
            Err(LibraryCatalogError::DefinitionKeyMismatch { .. })
        ));

        let mut wrong_revision = intent(10.0);
        wrong_revision.definition_revision += 1;
        assert!(matches!(
            resolve_builtin_insertion(&wrong_revision),
            Err(LibraryCatalogError::DefinitionRevisionMismatch { .. })
        ));

        let mut wrong_digest = intent(10.0);
        wrong_digest.definition_digest = "00".repeat(32);
        assert!(matches!(
            resolve_builtin_insertion(&wrong_digest),
            Err(LibraryCatalogError::DefinitionDigestMismatch { .. })
        ));

        let mut missing = intent(10.0);
        missing.parameters.clear();
        assert!(matches!(
            resolve_builtin_insertion(&missing),
            Err(LibraryCatalogError::MissingParameter(LENGTH_PARAMETER_KEY))
        ));

        let mut duplicate = intent(10.0);
        duplicate.parameters.push(duplicate.parameters[0].clone());
        assert!(matches!(
            resolve_builtin_insertion(&duplicate),
            Err(LibraryCatalogError::DuplicateParameter(_))
        ));

        let mut unexpected = intent(10.0);
        unexpected.parameters[0].key = "width".into();
        assert!(matches!(
            resolve_builtin_insertion(&unexpected),
            Err(LibraryCatalogError::UnexpectedParameter(_))
        ));
    }

    #[test]
    fn nonfinite_and_out_of_range_lengths_never_create_commands() {
        for value in [f64::NAN, f64::INFINITY, 0.0, -1.0, 100_000.001] {
            assert!(matches!(
                resolve_builtin_insertion(&intent(value)),
                Err(LibraryCatalogError::InvalidLength(_))
            ));
        }
    }
}

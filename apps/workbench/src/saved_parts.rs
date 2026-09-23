//! Parts a person saves into the Part Library from their own work (ADR 0053).
//!
//! A saved part is the document it was drawn in, with one of its bodies named
//! as the part, sealed into an immutable catalog package. Each variable that
//! holds a plain length, angle or number becomes one of the part's
//! parameters, defaulting to the value it had when the part was saved.
//!
//! Inserting the part evaluates that document again at the values given for
//! it: the same replay that opens a file, so an extrusion whose distance
//! follows `length` (ADR 0052) comes out at the new length. The body that
//! comes out is placed as a component, and the component keeps the chain of
//! kernel commands that built it, so the document it is placed in replays it
//! on its own, without the library.

use std::collections::BTreeMap;

use artificer_catalog::{
    CatalogStore, DisplayUnit, EmbeddedDocument, ParameterDomain,
    ParameterId as CatalogParameterId, ParameterSpec as CatalogParameterSpec, PartDefinition,
    PartDefinitionId, PartMetadata, PartPackage, PartRevision, RealQuantity, RealRules,
};
use artificer_kernel::{ExecutionOutcome, NativeKernel};
use artificer_model::{
    BodyId, CURRENT_DOCUMENT_VERSION, EvaluatedParameters, ModelDocument, ParameterBinding,
    ParameterOverrides, ParameterType, ParameterUnit, ParameterValue, QuantityKind, ReplayAction,
};
use artificer_protocol::{KernelCommand, PrecisionPolicy, SnapshotId};
use serde::{Deserialize, Serialize};

use crate::document_replay::{HydrationOptions, execute_chain, hydrate_model_document};
use crate::part_library::{LibraryParameter, LibraryPart, ParameterQuantity};

/// The media type a saved part's embedded document carries.
pub const SAVED_PART_MEDIA_TYPE: &str = "application/vnd.artificer.saved-part+json";
const SAVED_PART_FORMAT: &str = "artificer.saved-part";
const SAVED_PART_FORMAT_VERSION: u32 = 1;
/// The category saved parts are filed under.
pub const SAVED_PART_CATEGORY: &str = "My parts";
/// The prefix every saved part's definition key starts with.
const SAVED_PART_KEY_PREFIX: &str = "user.";

/// What goes inside a saved part's package: the document, and which of its
/// bodies is the part.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPartDocument {
    format: String,
    version: u32,
    body: BodyId,
    document: ModelDocument,
}

/// Why a part could not be saved or evaluated. Every message is one a person
/// can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedPartError {
    /// The document has no body to save.
    NoBody,
    /// More than one body could be the part and none is picked.
    WhichBody { count: usize },
    /// A name that makes no usable key.
    InvalidName(String),
    /// The part's body is combined from others, which a saved part cannot
    /// replay yet.
    CombinedBodies,
    /// The part's body does not start from nothing in this document.
    NotSelfContained,
    /// Anything the catalog, the model or the kernel refused.
    Refused(String),
}

impl std::fmt::Display for SavedPartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBody => write!(formatter, "there is no body to save as a part"),
            Self::WhichBody { count } => write!(
                formatter,
                "there are {count} visible bodies; select the one to save as the part"
            ),
            Self::InvalidName(reason) => write!(formatter, "{reason}"),
            Self::CombinedBodies => write!(
                formatter,
                "the body combines other bodies, which a saved part cannot rebuild yet"
            ),
            Self::NotSelfContained => write!(
                formatter,
                "the body is built on another body's result, which a saved part cannot rebuild yet"
            ),
            Self::Refused(reason) => write!(formatter, "{reason}"),
        }
    }
}

impl std::error::Error for SavedPartError {}

fn refused(error: impl std::fmt::Display) -> SavedPartError {
    SavedPartError::Refused(error.to_string())
}

/// A variable that can become a parameter of a saved part: it holds a plain
/// length, angle or number rather than an expression over other variables.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposableVariable {
    pub id: artificer_model::ParameterId,
    pub key: String,
    pub label: String,
    pub quantity: QuantityKind,
    /// Its value now, canonical: millimetres, radians, or the number.
    pub value: f64,
}

/// The variables of `document` that can become a saved part's parameters,
/// in the order the Variables panel lists them.
#[must_use]
pub fn exposable_variables(document: &ModelDocument) -> Vec<ExposableVariable> {
    let Ok(evaluated) = document.evaluate_parameters(&ParameterOverrides::default()) else {
        return Vec::new();
    };
    document
        .parameters()
        .records()
        .iter()
        .filter(|record| matches!(record.binding, ParameterBinding::Literal { .. }))
        .filter_map(|record| {
            let ParameterType::Quantity(quantity) = record.spec.value_type else {
                return None;
            };
            let ParameterValue::Quantity { value } = evaluated.get(record.id)? else {
                return None;
            };
            CatalogParameterId::parse(&record.spec.key).ok()?;
            Some(ExposableVariable {
                id: record.id,
                key: record.spec.key.clone(),
                label: record.spec.label.clone(),
                quantity,
                value: value.magnitude,
            })
        })
        .collect()
}

/// The key a part saved under `name` is kept under: `user.` and the name in
/// lower case, with anything but letters and digits made a hyphen.
pub fn definition_key_for(name: &str) -> Result<PartDefinitionId, SavedPartError> {
    let mut slug = String::new();
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let limit = artificer_catalog::MAX_IDENTIFIER_BYTES - SAVED_PART_KEY_PREFIX.len();
    let slug = &slug[..slug.len().min(limit)];
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        return Err(SavedPartError::InvalidName(
            "a part's name needs at least one letter or digit".into(),
        ));
    }
    PartDefinitionId::parse(format!("{SAVED_PART_KEY_PREFIX}{slug}")).map_err(refused)
}

/// The revision a new save of `key` takes: one major version past the
/// newest the library already has, or 1.0.0 for a new part.
pub fn next_revision(
    store: &CatalogStore,
    key: &PartDefinitionId,
) -> Result<PartRevision, SavedPartError> {
    let index = store.index_snapshot().map_err(refused)?;
    let newest = index
        .entries()
        .iter()
        .filter(|entry| entry.definition_id() == key)
        .map(|entry| entry.revision().major())
        .max()
        .unwrap_or(0);
    Ok(PartRevision::new(newest.saturating_add(1), 0, 0))
}

/// What to save.
#[derive(Clone, Debug, PartialEq)]
pub struct SaveRequest {
    pub name: String,
    pub description: Option<String>,
    /// The body that is the part.
    pub body: BodyId,
    /// The variables that become its parameters.
    pub parameters: Vec<artificer_model::ParameterId>,
}

const fn catalog_quantity(quantity: QuantityKind) -> RealQuantity {
    match quantity {
        QuantityKind::Length => RealQuantity::Length,
        QuantityKind::Angle => RealQuantity::Angle,
        QuantityKind::Scalar => RealQuantity::Scalar,
    }
}

const fn display_unit(quantity: QuantityKind, unit: Option<ParameterUnit>) -> DisplayUnit {
    match (quantity, unit) {
        (QuantityKind::Length, Some(ParameterUnit::Centimeter)) => DisplayUnit::Centimetre,
        (QuantityKind::Length, Some(ParameterUnit::Meter)) => DisplayUnit::Metre,
        (QuantityKind::Length, Some(ParameterUnit::Inch)) => DisplayUnit::Inch,
        (QuantityKind::Length, _) => DisplayUnit::Millimetre,
        (QuantityKind::Angle, Some(ParameterUnit::Radian)) => DisplayUnit::Radian,
        (QuantityKind::Angle, _) => DisplayUnit::Degree,
        (QuantityKind::Scalar, _) => DisplayUnit::Unitless,
    }
}

/// A stored quantity in canonical units: millimetres, radians, or the number.
fn canonical(value: &ParameterValue) -> Option<f64> {
    let ParameterValue::Quantity { value } = value else {
        return None;
    };
    let scale = match value.unit {
        ParameterUnit::Micrometer => 1.0e-3,
        ParameterUnit::Millimeter | ParameterUnit::Radian | ParameterUnit::Scalar => 1.0,
        ParameterUnit::Centimeter => 10.0,
        ParameterUnit::Meter => 1_000.0,
        ParameterUnit::Inch => 25.4,
        ParameterUnit::Foot => 304.8,
        ParameterUnit::Degree => std::f64::consts::PI / 180.0,
    };
    Some(value.magnitude * scale)
}

/// Seals `request` from `document` into a package, and proves it builds by
/// evaluating it once at its saved values.
pub fn package_part(
    document: &ModelDocument,
    request: &SaveRequest,
    key: PartDefinitionId,
    revision: PartRevision,
) -> Result<PartPackage, SavedPartError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(SavedPartError::InvalidName("a part needs a name".into()));
    }
    let mut document = document.clone();
    document
        .set_history_position(document.features().len())
        .map_err(refused)?;
    document.clear_undo_history();
    if document.body(request.body).is_none() {
        return Err(SavedPartError::NoBody);
    }
    let exposable = exposable_variables(&document);
    let mut parameters = Vec::new();
    for (order, id) in request.parameters.iter().enumerate() {
        let Some(variable) = exposable.iter().find(|variable| variable.id == *id) else {
            continue;
        };
        let record = document.parameter(*id).ok_or(SavedPartError::NoBody)?;
        let rules = RealRules::new(
            record.spec.metadata.minimum.as_ref().and_then(canonical),
            record.spec.metadata.maximum.as_ref().and_then(canonical),
            None,
        )
        .map_err(refused)?;
        let spec = CatalogParameterSpec::real(
            CatalogParameterId::parse(&variable.key).map_err(refused)?,
            variable.label.clone(),
            u32::try_from(order).unwrap_or(u32::MAX),
            catalog_quantity(variable.quantity),
            display_unit(variable.quantity, record.spec.display_unit),
            Some(variable.value),
            rules,
        )
        .map_err(refused)?;
        parameters.push(spec);
    }
    let embedded = serde_json::to_vec(&SavedPartDocument {
        format: SAVED_PART_FORMAT.to_owned(),
        version: SAVED_PART_FORMAT_VERSION,
        body: request.body,
        document,
    })
    .map_err(refused)?;
    let embedded =
        EmbeddedDocument::from_json(SAVED_PART_MEDIA_TYPE, CURRENT_DOCUMENT_VERSION, embedded)
            .map_err(refused)?;
    let mut metadata = PartMetadata::new(name)
        .and_then(|metadata| metadata.with_category(SAVED_PART_CATEGORY))
        .and_then(|metadata| metadata.with_tags(["saved"]))
        .map_err(refused)?;
    if let Some(description) = request
        .description
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        metadata = metadata.with_description(description).map_err(refused)?;
    }
    let definition = if parameters.is_empty() {
        PartDefinition::fixed(key, revision, metadata, embedded)
    } else {
        PartDefinition::parametric(key, revision, metadata, parameters, embedded)
    }
    .map_err(refused)?;
    let package = PartPackage::seal(definition).map_err(refused)?;
    evaluate_saved_part(&package, &BTreeMap::new())?;
    Ok(package)
}

/// Whether a package is a part saved from someone's own document.
#[must_use]
pub fn is_saved_part(package: &PartPackage) -> bool {
    package.definition().document().media_type() == SAVED_PART_MEDIA_TYPE
}

/// A saved part evaluated at one set of values.
pub struct EvaluatedPart {
    /// The kernel commands that build the part's body from nothing.
    pub commands: Vec<KernelCommand>,
    /// The last of them run: the body, and its report.
    pub outcome: ExecutionOutcome,
    /// The part's variables as they came out.
    pub evaluated: EvaluatedParameters,
}

/// The canonical value each of the package's parameters takes: the given one,
/// or its default.
fn parameter_values(
    package: &PartPackage,
    values: &BTreeMap<String, f64>,
) -> Result<BTreeMap<String, f64>, SavedPartError> {
    let mut resolved = BTreeMap::new();
    for spec in package.definition().parameters() {
        let key = spec.id().as_str().to_owned();
        let value = match (values.get(&key), spec.domain()) {
            (Some(value), _) => *value,
            (None, ParameterDomain::Real { default, .. }) => {
                default.map(|value| value.get()).ok_or_else(|| {
                    SavedPartError::Refused(format!("{} needs a value", spec.label()))
                })?
            }
            (None, _) => {
                return Err(SavedPartError::Refused(format!(
                    "{} is not a number parameter",
                    spec.label()
                )));
            }
        };
        resolved.insert(key, value);
    }
    Ok(resolved)
}

/// Evaluates a saved part at `values` (canonical, by parameter key; a
/// parameter left out takes its default), and returns the chain of commands
/// that builds its body, with the body.
pub fn evaluate_saved_part(
    package: &PartPackage,
    values: &BTreeMap<String, f64>,
) -> Result<EvaluatedPart, SavedPartError> {
    if !is_saved_part(package) {
        return Err(SavedPartError::Refused(
            "the package is not a saved part".into(),
        ));
    }
    let saved: SavedPartDocument =
        serde_json::from_str(package.definition().document().canonical_json()).map_err(refused)?;
    if saved.format != SAVED_PART_FORMAT || saved.version != SAVED_PART_FORMAT_VERSION {
        return Err(SavedPartError::Refused(format!(
            "saved part format {} {} is not one this build reads",
            saved.format, saved.version
        )));
    }
    let mut document = saved.document;
    for (key, value) in parameter_values(package, values)? {
        let record = document
            .parameters()
            .get_by_key(&key)
            .ok_or_else(|| SavedPartError::Refused(format!("the part has no variable {key}")))?;
        let unit = match record.spec.value_type {
            ParameterType::Quantity(QuantityKind::Length) => ParameterUnit::Millimeter,
            ParameterType::Quantity(QuantityKind::Angle) => ParameterUnit::Radian,
            _ => ParameterUnit::Scalar,
        };
        let id = record.id;
        document
            .set_parameter_binding(
                id,
                ParameterBinding::literal(ParameterValue::quantity(value, unit)),
            )
            .map_err(refused)?;
    }
    // A sketch value typed over a variable follows it here as it does in
    // the app (ADR 0054), so a sketch dimension is as much a part's
    // parameter as an extrusion distance.
    crate::sketch_links::follow_variables(&mut document, true).map_err(SavedPartError::Refused)?;
    let evaluated = document
        .evaluate_parameters(&ParameterOverrides::default())
        .map_err(refused)?;
    let body = saved.body;
    let hydrated =
        hydrate_model_document(document, HydrationOptions::default()).map_err(refused)?;
    let mut commands = Vec::new();
    let mut first_feature = None;
    for result in &hydrated.features {
        if !result.branches.contains(&body) {
            continue;
        }
        let action = hydrated
            .document
            .feature(result.feature)
            .map(|node| &node.action);
        if matches!(action, Some(ReplayAction::Boolean(_))) {
            return Err(SavedPartError::CombinedBodies);
        }
        if result.commands.is_empty() {
            continue;
        }
        if commands.is_empty() && result.association.input != SnapshotId::ZERO {
            return Err(SavedPartError::NotSelfContained);
        }
        first_feature.get_or_insert(result.feature);
        commands.extend(result.commands.iter().cloned());
    }
    let Some(first_feature) = first_feature else {
        return Err(SavedPartError::NoBody);
    };
    let expected = hydrated
        .branch_heads
        .get(&body)
        .copied()
        .ok_or(SavedPartError::NoBody)?;
    let outcome = execute_chain(
        first_feature,
        &NativeKernel::empty(),
        &commands,
        PrecisionPolicy::default(),
    )
    .map_err(refused)?;
    if outcome.snapshot.id() != expected {
        return Err(SavedPartError::Refused(
            "the part did not build the same way twice".into(),
        ));
    }
    Ok(EvaluatedPart {
        commands,
        outcome,
        evaluated,
    })
}

/// The replay action a placed saved part carries: its one command, or the
/// chain of them.
#[must_use]
pub fn replay_action(commands: Vec<KernelCommand>) -> ReplayAction {
    match <[KernelCommand; 1]>::try_from(commands) {
        Ok([command]) => ReplayAction::Kernel(command),
        Err(commands) => ReplayAction::KernelChain(commands),
    }
}

/// The body that would be saved as the part: the only visible body, or the
/// selected one when there are several.
pub fn part_body(visible: &[BodyId], selected: Option<BodyId>) -> Result<BodyId, SavedPartError> {
    match (visible, selected) {
        ([], _) => Err(SavedPartError::NoBody),
        ([only], _) => Ok(*only),
        (_, Some(selected)) if visible.contains(&selected) => Ok(selected),
        (many, _) => Err(SavedPartError::WhichBody { count: many.len() }),
    }
}

/// How the library lists a saved part: its identity, its words, and the
/// parameters it takes with their defaults and limits.
#[must_use]
pub fn library_part(package: &PartPackage) -> LibraryPart {
    let definition = package.definition();
    let revision = definition.revision();
    let metadata = definition.metadata();
    let parameters = definition
        .parameters()
        .iter()
        .filter_map(|spec| match spec.domain() {
            ParameterDomain::Real {
                quantity,
                default,
                rules,
                ..
            } => Some(LibraryParameter {
                key: spec.id().as_str().to_owned(),
                label: spec.label().to_owned(),
                quantity: match quantity {
                    RealQuantity::Length => ParameterQuantity::Length,
                    RealQuantity::Angle => ParameterQuantity::Angle,
                    RealQuantity::Scalar => ParameterQuantity::Number,
                },
                default: default.map(|value| value.get()),
                minimum: rules.minimum().map(|value| value.get()),
                maximum: rules.maximum().map(|value| value.get()),
            }),
            ParameterDomain::Integer { .. }
            | ParameterDomain::Boolean { .. }
            | ParameterDomain::Choice { .. } => None,
        })
        .collect::<Vec<_>>();
    LibraryPart {
        key: definition.id().as_str().to_owned(),
        revision: [revision.major(), revision.minor(), revision.patch()],
        digest: package.content_digest().to_hex(),
        name: metadata.name().to_owned(),
        description: metadata.description().map(str::to_owned),
        category: metadata.category().map(str::to_owned),
        parametric: !parameters.is_empty(),
        parameters,
        keywords: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_becomes_a_safe_key() {
        assert_eq!(definition_key_for("Bar").unwrap().as_str(), "user.bar");
        assert_eq!(
            definition_key_for("  20×20 Corner Bracket (v2) ")
                .unwrap()
                .as_str(),
            "user.20-20-corner-bracket-v2"
        );
        assert!(matches!(
            definition_key_for("×××"),
            Err(SavedPartError::InvalidName(_))
        ));
        let long = definition_key_for(&"x".repeat(200)).unwrap();
        assert!(long.as_str().len() <= artificer_catalog::MAX_IDENTIFIER_BYTES);
    }

    #[test]
    fn the_part_is_the_one_body_or_the_picked_one() {
        let serialized =
            |value: u64| -> BodyId { serde_json::from_str(&value.to_string()).unwrap() };
        let (a, b) = (serialized(1), serialized(2));
        assert_eq!(part_body(&[], None), Err(SavedPartError::NoBody));
        assert_eq!(part_body(&[a], None), Ok(a));
        assert_eq!(part_body(&[a], Some(b)), Ok(a), "the only visible body");
        assert_eq!(part_body(&[a, b], Some(b)), Ok(b));
        assert_eq!(
            part_body(&[a, b], None),
            Err(SavedPartError::WhichBody { count: 2 })
        );
        assert_eq!(
            SavedPartError::WhichBody { count: 2 }.to_string(),
            "there are 2 visible bodies; select the one to save as the part"
        );
    }
}

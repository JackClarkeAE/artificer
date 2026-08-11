//! Typed, deterministic parameters for native Artificer documents.
//!
//! Parameter expressions are deliberately declarative and bounded. They do
//! not execute scripts, access the environment, or depend on evaluation order.
//! All evaluated quantities are normalized to millimetres, radians, or a
//! dimensionless scalar before they reach a feature recipe.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ParameterId;

/// Maximum number of parameters retained by one native part document.
pub const MAX_PARAMETERS: usize = 1_024;
/// Maximum number of nodes in one parameter expression.
pub const MAX_EXPRESSION_NODES: usize = 256;
/// Maximum nesting depth in one parameter expression.
pub const MAX_EXPRESSION_DEPTH: usize = 32;
/// Maximum number of selectable values on one choice parameter.
pub const MAX_PARAMETER_CHOICES: usize = 128;
/// Maximum byte length of a stable machine-readable parameter key.
pub const MAX_PARAMETER_KEY_BYTES: usize = 64;
/// Maximum byte length of parameter descriptions.
pub const MAX_PARAMETER_DESCRIPTION_BYTES: usize = 512;

/// Physical dimension carried by a continuous parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantityKind {
    Length,
    Angle,
    Scalar,
}

/// Supported authoring/display units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterUnit {
    Micrometer,
    Millimeter,
    Centimeter,
    Meter,
    Inch,
    Foot,
    Radian,
    Degree,
    Scalar,
}

impl ParameterUnit {
    /// Physical dimension represented by this unit.
    #[must_use]
    pub const fn quantity_kind(self) -> QuantityKind {
        match self {
            Self::Micrometer
            | Self::Millimeter
            | Self::Centimeter
            | Self::Meter
            | Self::Inch
            | Self::Foot => QuantityKind::Length,
            Self::Radian | Self::Degree => QuantityKind::Angle,
            Self::Scalar => QuantityKind::Scalar,
        }
    }

    #[must_use]
    const fn canonical(self) -> Self {
        match self.quantity_kind() {
            QuantityKind::Length => Self::Millimeter,
            QuantityKind::Angle => Self::Radian,
            QuantityKind::Scalar => Self::Scalar,
        }
    }

    #[must_use]
    const fn canonical_scale(self) -> f64 {
        match self {
            Self::Micrometer => 0.001,
            Self::Millimeter | Self::Radian | Self::Scalar => 1.0,
            Self::Centimeter => 10.0,
            Self::Meter => 1_000.0,
            Self::Inch => 25.4,
            Self::Foot => 304.8,
            Self::Degree => std::f64::consts::PI / 180.0,
        }
    }
}

/// A finite continuous quantity. Evaluation normalizes this to its canonical
/// unit; authoring values may retain a convenient display unit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuantityValue {
    pub magnitude: f64,
    pub unit: ParameterUnit,
}

impl QuantityValue {
    #[must_use]
    pub const fn new(magnitude: f64, unit: ParameterUnit) -> Self {
        Self { magnitude, unit }
    }

    fn canonical(self) -> Result<Self, ParameterError> {
        if !self.magnitude.is_finite() {
            return Err(ParameterError::NonFinite);
        }
        let magnitude = normalize_zero(self.magnitude * self.unit.canonical_scale());
        if !magnitude.is_finite() {
            return Err(ParameterError::NonFinite);
        }
        Ok(Self {
            magnitude,
            unit: self.unit.canonical(),
        })
    }
}

/// Declared value type of a parameter or expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "quantity", rename_all = "snake_case")]
pub enum ParameterType {
    Quantity(QuantityKind),
    Integer,
    Boolean,
    Choice,
}

/// A concrete parameter value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParameterValue {
    Quantity { value: QuantityValue },
    Integer { value: i64 },
    Boolean { value: bool },
    Choice { value: String },
}

impl ParameterValue {
    #[must_use]
    pub const fn quantity(magnitude: f64, unit: ParameterUnit) -> Self {
        Self::Quantity {
            value: QuantityValue::new(magnitude, unit),
        }
    }

    #[must_use]
    pub const fn integer(value: i64) -> Self {
        Self::Integer { value }
    }

    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        Self::Boolean { value }
    }

    #[must_use]
    pub fn choice(value: impl Into<String>) -> Self {
        Self::Choice {
            value: value.into(),
        }
    }

    #[must_use]
    pub const fn value_type(&self) -> ParameterType {
        match self {
            Self::Quantity { value } => ParameterType::Quantity(value.unit.quantity_kind()),
            Self::Integer { .. } => ParameterType::Integer,
            Self::Boolean { .. } => ParameterType::Boolean,
            Self::Choice { .. } => ParameterType::Choice,
        }
    }

    fn canonical(&self) -> Result<Self, ParameterError> {
        match self {
            Self::Quantity { value } => Ok(Self::Quantity {
                value: value.canonical()?,
            }),
            Self::Integer { value } => Ok(Self::integer(*value)),
            Self::Boolean { value } => Ok(Self::boolean(*value)),
            Self::Choice { value } => {
                validate_choice_key(value)?;
                Ok(Self::choice(value))
            }
        }
    }
}

/// One user-visible option on an enumerated parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterChoice {
    pub value: String,
    pub label: String,
}

impl ParameterChoice {
    #[must_use]
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// Whether a parameter is an internal construction value or part of the
/// published part interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterExposure {
    #[default]
    Internal,
    UserInput,
}

/// Optional UI and validation metadata belonging to a parameter definition.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParameterMetadata {
    #[serde(default)]
    pub default_value: Option<ParameterValue>,
    #[serde(default)]
    pub minimum: Option<ParameterValue>,
    #[serde(default)]
    pub maximum: Option<ParameterValue>,
    #[serde(default)]
    pub step: Option<ParameterValue>,
    #[serde(default)]
    pub choices: Vec<ParameterChoice>,
    #[serde(default)]
    pub exposure: ParameterExposure,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub order: u32,
}

/// Definition of one typed parameter, excluding its stable document ID and
/// current binding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParameterSpec {
    pub key: String,
    pub label: String,
    pub value_type: ParameterType,
    #[serde(default)]
    pub display_unit: Option<ParameterUnit>,
    #[serde(default)]
    pub metadata: ParameterMetadata,
}

impl ParameterSpec {
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        value_type: ParameterType,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            value_type,
            display_unit: None,
            metadata: ParameterMetadata::default(),
        }
    }

    #[must_use]
    pub const fn with_display_unit(mut self, unit: ParameterUnit) -> Self {
        self.display_unit = Some(unit);
        self
    }

    #[must_use]
    pub fn with_metadata(mut self, metadata: ParameterMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Bounded, declarative expression tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ParameterExpression {
    Literal { value: ParameterValue },
    Reference { parameter: ParameterId },
    Negate { operand: Box<Self> },
    Add { left: Box<Self>, right: Box<Self> },
    Subtract { left: Box<Self>, right: Box<Self> },
    Multiply { left: Box<Self>, right: Box<Self> },
    Divide { left: Box<Self>, right: Box<Self> },
}

impl ParameterExpression {
    #[must_use]
    pub fn literal(value: ParameterValue) -> Self {
        Self::Literal { value }
    }

    #[must_use]
    pub const fn reference(parameter: ParameterId) -> Self {
        Self::Reference { parameter }
    }

    fn references(&self, output: &mut BTreeSet<ParameterId>) {
        match self {
            Self::Reference { parameter } => {
                output.insert(*parameter);
            }
            Self::Negate { operand } => operand.references(output),
            Self::Add { left, right }
            | Self::Subtract { left, right }
            | Self::Multiply { left, right }
            | Self::Divide { left, right } => {
                left.references(output);
                right.references(output);
            }
            Self::Literal { .. } => {}
        }
    }

    fn validate_bounds(&self) -> Result<(), ParameterError> {
        fn visit(
            expression: &ParameterExpression,
            depth: usize,
            nodes: &mut usize,
        ) -> Result<(), ParameterError> {
            if depth > MAX_EXPRESSION_DEPTH {
                return Err(ParameterError::ExpressionTooDeep);
            }
            *nodes += 1;
            if *nodes > MAX_EXPRESSION_NODES {
                return Err(ParameterError::ExpressionTooLarge);
            }
            match expression {
                ParameterExpression::Literal { value } => {
                    value.canonical()?;
                }
                ParameterExpression::Reference { parameter } => {
                    if parameter.get() == 0 {
                        return Err(ParameterError::InvalidStableId);
                    }
                }
                ParameterExpression::Negate { operand } => visit(operand, depth + 1, nodes)?,
                ParameterExpression::Add { left, right }
                | ParameterExpression::Subtract { left, right }
                | ParameterExpression::Multiply { left, right }
                | ParameterExpression::Divide { left, right } => {
                    visit(left, depth + 1, nodes)?;
                    visit(right, depth + 1, nodes)?;
                }
            }
            Ok(())
        }

        visit(self, 1, &mut 0)
    }
}

/// Current authored source for a parameter value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ParameterBinding {
    #[default]
    Unresolved,
    Literal {
        value: ParameterValue,
    },
    Expression {
        expression: ParameterExpression,
    },
}

impl ParameterBinding {
    #[must_use]
    pub fn literal(value: ParameterValue) -> Self {
        Self::Literal { value }
    }

    #[must_use]
    pub fn expression(expression: ParameterExpression) -> Self {
        Self::Expression { expression }
    }

    fn references(&self) -> BTreeSet<ParameterId> {
        let mut references = BTreeSet::new();
        if let Self::Expression { expression } = self {
            expression.references(&mut references);
        }
        references
    }
}

/// Persisted parameter definition and current authored binding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParameterRecord {
    pub id: ParameterId,
    pub spec: ParameterSpec,
    #[serde(default)]
    pub binding: ParameterBinding,
}

impl ParameterRecord {
    /// True when the published interface needs a value from the user before
    /// this parameter can be evaluated.
    #[must_use]
    pub fn is_required_input(&self) -> bool {
        self.spec.metadata.exposure == ParameterExposure::UserInput
            && self.spec.metadata.default_value.is_none()
            && matches!(self.binding, ParameterBinding::Unresolved)
    }
}

/// Ordered stable parameter table. Deserialization validates all identities,
/// metadata, references, expression bounds, types, and dependency cycles.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ParameterTable {
    parameters: Vec<ParameterRecord>,
}

#[derive(Deserialize)]
struct RawParameterTable {
    #[serde(default)]
    parameters: Vec<ParameterRecord>,
}

impl<'de> Deserialize<'de> for ParameterTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawParameterTable::deserialize(deserializer)?;
        Self::try_from_records(raw.parameters).map_err(de::Error::custom)
    }
}

impl ParameterTable {
    /// Constructs and validates a table from persisted records.
    pub fn try_from_records(parameters: Vec<ParameterRecord>) -> Result<Self, ParameterError> {
        let table = Self { parameters };
        table.validate()?;
        Ok(table)
    }

    #[must_use]
    pub fn records(&self) -> &[ParameterRecord] {
        &self.parameters
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.parameters.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: ParameterId) -> Option<&ParameterRecord> {
        self.parameters.iter().find(|record| record.id == id)
    }

    #[must_use]
    pub fn get_by_key(&self, key: &str) -> Option<&ParameterRecord> {
        self.parameters.iter().find(|record| record.spec.key == key)
    }

    /// Parameters directly or transitively derived from `source`, including
    /// `source` itself. This is the dirty-propagation boundary for features.
    #[must_use]
    pub fn affected_by(&self, source: ParameterId) -> BTreeSet<ParameterId> {
        let mut affected = BTreeSet::from([source]);
        loop {
            let previous_len = affected.len();
            for record in &self.parameters {
                if record
                    .binding
                    .references()
                    .iter()
                    .any(|reference| affected.contains(reference))
                {
                    affected.insert(record.id);
                }
            }
            if affected.len() == previous_len {
                return affected;
            }
        }
    }

    pub(crate) fn insert_allocated(
        &mut self,
        id: ParameterId,
        spec: ParameterSpec,
        binding: ParameterBinding,
    ) -> Result<(), ParameterError> {
        let mut candidate = self.clone();
        candidate
            .parameters
            .push(ParameterRecord { id, spec, binding });
        candidate.parameters.sort_by_key(|record| record.id);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub(crate) fn replace_spec(
        &mut self,
        id: ParameterId,
        spec: ParameterSpec,
    ) -> Result<bool, ParameterError> {
        let mut candidate = self.clone();
        let record = candidate
            .parameters
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or(ParameterError::UnknownParameter(id))?;
        if record.spec == spec {
            return Ok(false);
        }
        record.spec = spec;
        candidate.validate()?;
        *self = candidate;
        Ok(true)
    }

    pub(crate) fn set_binding(
        &mut self,
        id: ParameterId,
        binding: ParameterBinding,
    ) -> Result<bool, ParameterError> {
        let mut candidate = self.clone();
        let record = candidate
            .parameters
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or(ParameterError::UnknownParameter(id))?;
        if record.binding == binding {
            return Ok(false);
        }
        record.binding = binding;
        candidate.validate()?;
        *self = candidate;
        Ok(true)
    }

    pub(crate) fn remove(&mut self, id: ParameterId) -> Result<ParameterRecord, ParameterError> {
        let mut candidate = self.clone();
        let index = candidate
            .parameters
            .iter()
            .position(|record| record.id == id)
            .ok_or(ParameterError::UnknownParameter(id))?;
        let removed = candidate.parameters.remove(index);
        candidate.validate()?;
        *self = candidate;
        Ok(removed)
    }

    /// Resolves all parameter values, applying explicit overrides first,
    /// authored bindings second, and defaults last.
    pub fn evaluate(
        &self,
        overrides: &ParameterOverrides,
    ) -> Result<EvaluatedParameters, ParameterError> {
        self.validate()?;
        overrides.validate()?;
        for id in overrides.values.keys() {
            if self.get(*id).is_none() {
                return Err(ParameterError::UnknownParameter(*id));
            }
        }

        let mut evaluator = Evaluator {
            table: self,
            overrides,
            values: BTreeMap::new(),
            visiting: Vec::new(),
        };
        let ids = self
            .parameters
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        for id in ids {
            evaluator.evaluate_parameter(id)?;
        }
        Ok(EvaluatedParameters {
            values: evaluator.values,
        })
    }

    /// Validates persisted structure without requiring unresolved published
    /// inputs to have values.
    pub fn validate(&self) -> Result<(), ParameterError> {
        if self.parameters.len() > MAX_PARAMETERS {
            return Err(ParameterError::CapacityExceeded);
        }
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for record in &self.parameters {
            if record.id.get() == 0 {
                return Err(ParameterError::InvalidStableId);
            }
            if !ids.insert(record.id) {
                return Err(ParameterError::DuplicateId(record.id));
            }
            validate_parameter_key(&record.spec.key)?;
            if !keys.insert(record.spec.key.clone()) {
                return Err(ParameterError::DuplicateKey(record.spec.key.clone()));
            }
            validate_spec(record.id, &record.spec)?;
            validate_binding_bounds(&record.binding)?;
            if let ParameterBinding::Literal { value } = &record.binding {
                validate_value_against_spec(record.id, &record.spec, value)?;
            }
        }
        validate_dependencies_and_types(self)?;

        // A required unresolved input is a valid authoring state. Every value
        // that can be resolved without such an input must nevertheless satisfy
        // arithmetic and range constraints before the table is persisted.
        let overrides = ParameterOverrides::default();
        for record in &self.parameters {
            let mut evaluator = Evaluator {
                table: self,
                overrides: &overrides,
                values: BTreeMap::new(),
                visiting: Vec::new(),
            };
            match evaluator.evaluate_parameter(record.id) {
                Ok(_) | Err(ParameterError::MissingValue(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Concrete values supplied when resolving a published part variant.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ParameterOverrides {
    values: BTreeMap<ParameterId, ParameterValue>,
}

#[derive(Deserialize)]
struct RawParameterOverrides {
    #[serde(default)]
    values: BTreeMap<ParameterId, ParameterValue>,
}

impl<'de> Deserialize<'de> for ParameterOverrides {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawParameterOverrides::deserialize(deserializer)?;
        let overrides = Self { values: raw.values };
        overrides.validate().map_err(de::Error::custom)?;
        Ok(overrides)
    }
}

impl ParameterOverrides {
    #[must_use]
    pub fn values(&self) -> &BTreeMap<ParameterId, ParameterValue> {
        &self.values
    }

    pub fn set(&mut self, id: ParameterId, value: ParameterValue) -> Result<(), ParameterError> {
        if id.get() == 0 {
            return Err(ParameterError::InvalidStableId);
        }
        let value = value.canonical()?;
        let mut candidate = self.clone();
        candidate.values.insert(id, value);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn remove(&mut self, id: ParameterId) -> Option<ParameterValue> {
        self.values.remove(&id)
    }

    fn validate(&self) -> Result<(), ParameterError> {
        if self.values.len() > MAX_PARAMETERS {
            return Err(ParameterError::CapacityExceeded);
        }
        for (id, value) in &self.values {
            if id.get() == 0 {
                return Err(ParameterError::InvalidStableId);
            }
            value.canonical()?;
        }
        Ok(())
    }
}

/// Fully evaluated, canonical parameter assignment.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EvaluatedParameters {
    values: BTreeMap<ParameterId, ParameterValue>,
}

#[derive(Deserialize)]
struct RawEvaluatedParameters {
    #[serde(default)]
    values: BTreeMap<ParameterId, ParameterValue>,
}

impl<'de> Deserialize<'de> for EvaluatedParameters {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawEvaluatedParameters::deserialize(deserializer)?;
        Self::try_from_values(raw.values).map_err(de::Error::custom)
    }
}

impl EvaluatedParameters {
    /// Constructs a canonical resolved assignment from concrete values.
    pub fn try_from_values(
        values: BTreeMap<ParameterId, ParameterValue>,
    ) -> Result<Self, ParameterError> {
        if values.len() > MAX_PARAMETERS {
            return Err(ParameterError::CapacityExceeded);
        }
        let mut canonical = BTreeMap::new();
        for (id, value) in values {
            if id.get() == 0 {
                return Err(ParameterError::InvalidStableId);
            }
            canonical.insert(id, value.canonical()?);
        }
        Ok(Self { values: canonical })
    }

    #[must_use]
    pub fn values(&self) -> &BTreeMap<ParameterId, ParameterValue> {
        &self.values
    }

    #[must_use]
    pub fn get(&self, id: ParameterId) -> Option<&ParameterValue> {
        self.values.get(&id)
    }

    /// Content digest used to key a resolved part-variant cache.
    #[must_use]
    pub fn binding_digest(&self) -> ParameterBindingDigest {
        let mut hasher = Sha256::new();
        hasher.update(b"artificer.parameter-bindings.v1\0");
        hasher.update((self.values.len() as u64).to_be_bytes());
        for (id, value) in &self.values {
            hasher.update(id.get().to_be_bytes());
            hash_value(&mut hasher, value);
        }
        ParameterBindingDigest(hasher.finalize().into())
    }
}

/// SHA-256 digest of a canonical evaluated parameter assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParameterBindingDigest([u8; 32]);

impl ParameterBindingDigest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ParameterBindingDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for ParameterBindingDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ParameterBindingDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() != 64 {
            return Err(de::Error::custom(
                "parameter digest must contain 64 hex digits",
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = decode_hex(pair[0])
                .and_then(|high| decode_hex(pair[1]).map(|low| high << 4 | low))
                .ok_or_else(|| de::Error::custom("parameter digest contains invalid hex"))?;
        }
        Ok(Self(bytes))
    }
}

/// Structured, non-mutating parameter rejection.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ParameterError {
    #[error("parameter table exceeds the limit of {MAX_PARAMETERS}")]
    CapacityExceeded,
    #[error("stable parameter IDs must be non-zero")]
    InvalidStableId,
    #[error("duplicate parameter ID {0}")]
    DuplicateId(ParameterId),
    #[error("duplicate parameter key {0:?}")]
    DuplicateKey(String),
    #[error("parameter key is invalid")]
    InvalidKey,
    #[error("parameter label or metadata text is invalid")]
    InvalidText,
    #[error("unknown parameter {0}")]
    UnknownParameter(ParameterId),
    #[error("parameter {0} has no binding or default")]
    MissingValue(ParameterId),
    #[error("parameter dependency cycle: {path:?}")]
    DependencyCycle { path: Vec<ParameterId> },
    #[error("parameter expression exceeds {MAX_EXPRESSION_NODES} nodes")]
    ExpressionTooLarge,
    #[error("parameter expression exceeds nesting depth {MAX_EXPRESSION_DEPTH}")]
    ExpressionTooDeep,
    #[error("parameter {parameter} expects {expected:?}, received {actual:?}")]
    TypeMismatch {
        parameter: ParameterId,
        expected: ParameterType,
        actual: ParameterType,
    },
    #[error("parameter expression combines incompatible types {left:?} and {right:?}")]
    IncompatibleOperands {
        left: ParameterType,
        right: ParameterType,
    },
    #[error("parameter display unit is incompatible with its declared type")]
    UnitMismatch,
    #[error("parameter metadata is inconsistent")]
    InvalidMetadata,
    #[error("parameter quantity or expression result is not finite")]
    NonFinite,
    #[error("parameter expression divides by zero")]
    DivisionByZero,
    #[error("integer parameter expression overflowed")]
    IntegerOverflow,
    #[error("parameter {0} is outside its permitted range")]
    OutOfRange(ParameterId),
    #[error("parameter {parameter} does not allow choice {value:?}")]
    InvalidChoice {
        parameter: ParameterId,
        value: String,
    },
}

struct Evaluator<'a> {
    table: &'a ParameterTable,
    overrides: &'a ParameterOverrides,
    values: BTreeMap<ParameterId, ParameterValue>,
    visiting: Vec<ParameterId>,
}

impl Evaluator<'_> {
    fn evaluate_parameter(&mut self, id: ParameterId) -> Result<ParameterValue, ParameterError> {
        if let Some(value) = self.values.get(&id) {
            return Ok(value.clone());
        }
        if let Some(start) = self.visiting.iter().position(|candidate| *candidate == id) {
            let mut path = self.visiting[start..].to_vec();
            path.push(id);
            return Err(ParameterError::DependencyCycle { path });
        }
        let record = self
            .table
            .get(id)
            .ok_or(ParameterError::UnknownParameter(id))?;
        self.visiting.push(id);
        let value = if let Some(value) = self.overrides.values.get(&id) {
            value.canonical()?
        } else {
            match &record.binding {
                ParameterBinding::Unresolved => record
                    .spec
                    .metadata
                    .default_value
                    .as_ref()
                    .ok_or(ParameterError::MissingValue(id))?
                    .canonical()?,
                ParameterBinding::Literal { value } => value.canonical()?,
                ParameterBinding::Expression { expression } => {
                    self.evaluate_expression(expression)?
                }
            }
        };
        ensure_type(id, record.spec.value_type, &value)?;
        validate_value_against_spec(id, &record.spec, &value)?;
        self.visiting.pop();
        self.values.insert(id, value.clone());
        Ok(value)
    }

    fn evaluate_expression(
        &mut self,
        expression: &ParameterExpression,
    ) -> Result<ParameterValue, ParameterError> {
        match expression {
            ParameterExpression::Literal { value } => value.canonical(),
            ParameterExpression::Reference { parameter } => self.evaluate_parameter(*parameter),
            ParameterExpression::Negate { operand } => negate(self.evaluate_expression(operand)?),
            ParameterExpression::Add { left, right } => arithmetic(
                Arithmetic::Add,
                self.evaluate_expression(left)?,
                self.evaluate_expression(right)?,
            ),
            ParameterExpression::Subtract { left, right } => arithmetic(
                Arithmetic::Subtract,
                self.evaluate_expression(left)?,
                self.evaluate_expression(right)?,
            ),
            ParameterExpression::Multiply { left, right } => arithmetic(
                Arithmetic::Multiply,
                self.evaluate_expression(left)?,
                self.evaluate_expression(right)?,
            ),
            ParameterExpression::Divide { left, right } => arithmetic(
                Arithmetic::Divide,
                self.evaluate_expression(left)?,
                self.evaluate_expression(right)?,
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
}

fn arithmetic(
    operation: Arithmetic,
    left: ParameterValue,
    right: ParameterValue,
) -> Result<ParameterValue, ParameterError> {
    match (left, right) {
        (ParameterValue::Integer { value: left }, ParameterValue::Integer { value: right }) => {
            let value = match operation {
                Arithmetic::Add => left.checked_add(right),
                Arithmetic::Subtract => left.checked_sub(right),
                Arithmetic::Multiply => left.checked_mul(right),
                Arithmetic::Divide if right == 0 => return Err(ParameterError::DivisionByZero),
                Arithmetic::Divide => left.checked_div(right),
            }
            .ok_or(ParameterError::IntegerOverflow)?;
            Ok(ParameterValue::integer(value))
        }
        (ParameterValue::Quantity { value: left }, ParameterValue::Quantity { value: right }) => {
            quantity_arithmetic(operation, left.canonical()?, right.canonical()?)
        }
        (left, right) => Err(ParameterError::IncompatibleOperands {
            left: left.value_type(),
            right: right.value_type(),
        }),
    }
}

fn quantity_arithmetic(
    operation: Arithmetic,
    left: QuantityValue,
    right: QuantityValue,
) -> Result<ParameterValue, ParameterError> {
    let left_kind = left.unit.quantity_kind();
    let right_kind = right.unit.quantity_kind();
    let (magnitude, kind) = match operation {
        Arithmetic::Add if left_kind == right_kind => (left.magnitude + right.magnitude, left_kind),
        Arithmetic::Subtract if left_kind == right_kind => {
            (left.magnitude - right.magnitude, left_kind)
        }
        Arithmetic::Multiply if left_kind == QuantityKind::Scalar => {
            (left.magnitude * right.magnitude, right_kind)
        }
        Arithmetic::Multiply if right_kind == QuantityKind::Scalar => {
            (left.magnitude * right.magnitude, left_kind)
        }
        Arithmetic::Divide if right.magnitude == 0.0 => {
            return Err(ParameterError::DivisionByZero);
        }
        Arithmetic::Divide if right_kind == QuantityKind::Scalar => {
            (left.magnitude / right.magnitude, left_kind)
        }
        Arithmetic::Divide if left_kind == right_kind => {
            (left.magnitude / right.magnitude, QuantityKind::Scalar)
        }
        _ => {
            return Err(ParameterError::IncompatibleOperands {
                left: ParameterType::Quantity(left_kind),
                right: ParameterType::Quantity(right_kind),
            });
        }
    };
    if !magnitude.is_finite() {
        return Err(ParameterError::NonFinite);
    }
    let unit = match kind {
        QuantityKind::Length => ParameterUnit::Millimeter,
        QuantityKind::Angle => ParameterUnit::Radian,
        QuantityKind::Scalar => ParameterUnit::Scalar,
    };
    Ok(ParameterValue::quantity(normalize_zero(magnitude), unit))
}

fn negate(value: ParameterValue) -> Result<ParameterValue, ParameterError> {
    match value {
        ParameterValue::Integer { value } => value
            .checked_neg()
            .map(ParameterValue::integer)
            .ok_or(ParameterError::IntegerOverflow),
        ParameterValue::Quantity { value } => {
            ParameterValue::quantity(-value.magnitude, value.unit).canonical()
        }
        other => Err(ParameterError::IncompatibleOperands {
            left: other.value_type(),
            right: other.value_type(),
        }),
    }
}

fn validate_dependencies_and_types(table: &ParameterTable) -> Result<(), ParameterError> {
    let by_id = table
        .parameters
        .iter()
        .map(|record| (record.id, record))
        .collect::<BTreeMap<_, _>>();
    let mut permanent = BTreeSet::new();
    let mut temporary = Vec::new();

    fn visit(
        id: ParameterId,
        by_id: &BTreeMap<ParameterId, &ParameterRecord>,
        permanent: &mut BTreeSet<ParameterId>,
        temporary: &mut Vec<ParameterId>,
    ) -> Result<(), ParameterError> {
        if permanent.contains(&id) {
            return Ok(());
        }
        if let Some(start) = temporary.iter().position(|candidate| *candidate == id) {
            let mut path = temporary[start..].to_vec();
            path.push(id);
            return Err(ParameterError::DependencyCycle { path });
        }
        let record = by_id.get(&id).ok_or(ParameterError::UnknownParameter(id))?;
        temporary.push(id);
        for dependency in record.binding.references() {
            if !by_id.contains_key(&dependency) {
                return Err(ParameterError::UnknownParameter(dependency));
            }
            visit(dependency, by_id, permanent, temporary)?;
        }
        temporary.pop();
        permanent.insert(id);
        Ok(())
    }

    for id in by_id.keys().copied() {
        visit(id, &by_id, &mut permanent, &mut temporary)?;
    }
    for record in &table.parameters {
        if let ParameterBinding::Expression { expression } = &record.binding {
            let actual = infer_type(expression, &by_id)?;
            if actual != record.spec.value_type {
                return Err(ParameterError::TypeMismatch {
                    parameter: record.id,
                    expected: record.spec.value_type,
                    actual,
                });
            }
        }
    }
    Ok(())
}

fn infer_type(
    expression: &ParameterExpression,
    by_id: &BTreeMap<ParameterId, &ParameterRecord>,
) -> Result<ParameterType, ParameterError> {
    match expression {
        ParameterExpression::Literal { value } => Ok(value.value_type()),
        ParameterExpression::Reference { parameter } => by_id
            .get(parameter)
            .map(|record| record.spec.value_type)
            .ok_or(ParameterError::UnknownParameter(*parameter)),
        ParameterExpression::Negate { operand } => {
            let operand = infer_type(operand, by_id)?;
            if matches!(operand, ParameterType::Quantity(_) | ParameterType::Integer) {
                Ok(operand)
            } else {
                Err(ParameterError::IncompatibleOperands {
                    left: operand,
                    right: operand,
                })
            }
        }
        ParameterExpression::Add { left, right }
        | ParameterExpression::Subtract { left, right } => {
            let left = infer_type(left, by_id)?;
            let right = infer_type(right, by_id)?;
            if left == right && matches!(left, ParameterType::Quantity(_) | ParameterType::Integer)
            {
                Ok(left)
            } else {
                Err(ParameterError::IncompatibleOperands { left, right })
            }
        }
        ParameterExpression::Multiply { left, right } => {
            infer_multiply(infer_type(left, by_id)?, infer_type(right, by_id)?)
        }
        ParameterExpression::Divide { left, right } => {
            infer_divide(infer_type(left, by_id)?, infer_type(right, by_id)?)
        }
    }
}

fn infer_multiply(
    left: ParameterType,
    right: ParameterType,
) -> Result<ParameterType, ParameterError> {
    match (left, right) {
        (ParameterType::Integer, ParameterType::Integer) => Ok(ParameterType::Integer),
        (ParameterType::Quantity(QuantityKind::Scalar), other @ ParameterType::Quantity(_))
        | (other @ ParameterType::Quantity(_), ParameterType::Quantity(QuantityKind::Scalar)) => {
            Ok(other)
        }
        _ => Err(ParameterError::IncompatibleOperands { left, right }),
    }
}

fn infer_divide(
    left: ParameterType,
    right: ParameterType,
) -> Result<ParameterType, ParameterError> {
    match (left, right) {
        (ParameterType::Integer, ParameterType::Integer) => Ok(ParameterType::Integer),
        (left @ ParameterType::Quantity(_), ParameterType::Quantity(QuantityKind::Scalar)) => {
            Ok(left)
        }
        (ParameterType::Quantity(left), ParameterType::Quantity(right)) if left == right => {
            Ok(ParameterType::Quantity(QuantityKind::Scalar))
        }
        _ => Err(ParameterError::IncompatibleOperands { left, right }),
    }
}

fn validate_spec(id: ParameterId, spec: &ParameterSpec) -> Result<(), ParameterError> {
    validate_text(&spec.label, crate::MAX_LABEL_BYTES)?;
    if let Some(description) = &spec.metadata.description {
        validate_text(description, MAX_PARAMETER_DESCRIPTION_BYTES)?;
    }
    if let Some(group) = &spec.metadata.group {
        validate_text(group, crate::MAX_LABEL_BYTES)?;
    }
    match (spec.value_type, spec.display_unit) {
        (ParameterType::Quantity(kind), Some(unit)) if kind == unit.quantity_kind() => {}
        (ParameterType::Quantity(_), None) => {}
        (ParameterType::Quantity(_), Some(_)) | (_, Some(_)) => {
            return Err(ParameterError::UnitMismatch);
        }
        (_, None) => {}
    }

    for value in [
        spec.metadata.default_value.as_ref(),
        spec.metadata.minimum.as_ref(),
        spec.metadata.maximum.as_ref(),
        spec.metadata.step.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        ensure_type(id, spec.value_type, value)?;
        value.canonical()?;
    }

    match spec.value_type {
        ParameterType::Choice => {
            if spec.metadata.choices.is_empty()
                || spec.metadata.choices.len() > MAX_PARAMETER_CHOICES
                || spec.metadata.minimum.is_some()
                || spec.metadata.maximum.is_some()
                || spec.metadata.step.is_some()
            {
                return Err(ParameterError::InvalidMetadata);
            }
            let mut values = BTreeSet::new();
            for choice in &spec.metadata.choices {
                validate_choice_key(&choice.value)?;
                validate_text(&choice.label, crate::MAX_LABEL_BYTES)?;
                if !values.insert(&choice.value) {
                    return Err(ParameterError::InvalidMetadata);
                }
            }
        }
        ParameterType::Boolean => {
            if !spec.metadata.choices.is_empty()
                || spec.metadata.minimum.is_some()
                || spec.metadata.maximum.is_some()
                || spec.metadata.step.is_some()
            {
                return Err(ParameterError::InvalidMetadata);
            }
        }
        ParameterType::Quantity(_) | ParameterType::Integer => {
            if !spec.metadata.choices.is_empty() {
                return Err(ParameterError::InvalidMetadata);
            }
        }
    }

    if let (Some(minimum), Some(maximum)) = (&spec.metadata.minimum, &spec.metadata.maximum)
        && compare_numeric(minimum, maximum)? == std::cmp::Ordering::Greater
    {
        return Err(ParameterError::InvalidMetadata);
    }
    if let Some(step) = &spec.metadata.step
        && !is_positive(step)?
    {
        return Err(ParameterError::InvalidMetadata);
    }
    if let Some(default) = &spec.metadata.default_value {
        validate_value_against_spec(id, spec, default)?;
    }
    Ok(())
}

fn validate_binding_bounds(binding: &ParameterBinding) -> Result<(), ParameterError> {
    match binding {
        ParameterBinding::Unresolved => Ok(()),
        ParameterBinding::Literal { value } => {
            value.canonical()?;
            Ok(())
        }
        ParameterBinding::Expression { expression } => expression.validate_bounds(),
    }
}

fn validate_value_against_spec(
    id: ParameterId,
    spec: &ParameterSpec,
    value: &ParameterValue,
) -> Result<(), ParameterError> {
    ensure_type(id, spec.value_type, value)?;
    let value = value.canonical()?;
    if let Some(minimum) = &spec.metadata.minimum
        && compare_numeric(&value, &minimum.canonical()?)? == std::cmp::Ordering::Less
    {
        return Err(ParameterError::OutOfRange(id));
    }
    if let Some(maximum) = &spec.metadata.maximum
        && compare_numeric(&value, &maximum.canonical()?)? == std::cmp::Ordering::Greater
    {
        return Err(ParameterError::OutOfRange(id));
    }
    if let ParameterValue::Choice { value } = value
        && !spec
            .metadata
            .choices
            .iter()
            .any(|choice| choice.value == value)
    {
        return Err(ParameterError::InvalidChoice {
            parameter: id,
            value,
        });
    }
    Ok(())
}

fn ensure_type(
    parameter: ParameterId,
    expected: ParameterType,
    value: &ParameterValue,
) -> Result<(), ParameterError> {
    let actual = value.value_type();
    if expected == actual {
        Ok(())
    } else {
        Err(ParameterError::TypeMismatch {
            parameter,
            expected,
            actual,
        })
    }
}

fn compare_numeric(
    left: &ParameterValue,
    right: &ParameterValue,
) -> Result<std::cmp::Ordering, ParameterError> {
    match (left.canonical()?, right.canonical()?) {
        (ParameterValue::Quantity { value: left }, ParameterValue::Quantity { value: right })
            if left.unit == right.unit =>
        {
            left.magnitude
                .partial_cmp(&right.magnitude)
                .ok_or(ParameterError::NonFinite)
        }
        (ParameterValue::Integer { value: left }, ParameterValue::Integer { value: right }) => {
            Ok(left.cmp(&right))
        }
        (left, right) => Err(ParameterError::IncompatibleOperands {
            left: left.value_type(),
            right: right.value_type(),
        }),
    }
}

fn is_positive(value: &ParameterValue) -> Result<bool, ParameterError> {
    match value.canonical()? {
        ParameterValue::Quantity { value } => Ok(value.magnitude > 0.0),
        ParameterValue::Integer { value } => Ok(value > 0),
        other => Err(ParameterError::IncompatibleOperands {
            left: other.value_type(),
            right: other.value_type(),
        }),
    }
}

fn validate_parameter_key(key: &str) -> Result<(), ParameterError> {
    if key.is_empty() || key.len() > MAX_PARAMETER_KEY_BYTES {
        return Err(ParameterError::InvalidKey);
    }
    let mut bytes = key.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return Err(ParameterError::InvalidKey);
    }
    Ok(())
}

fn validate_choice_key(key: &str) -> Result<(), ParameterError> {
    if key.is_empty() || key.len() > MAX_PARAMETER_KEY_BYTES || key.chars().any(char::is_control) {
        Err(ParameterError::InvalidMetadata)
    } else {
        Ok(())
    }
}

fn validate_text(text: &str, max_bytes: usize) -> Result<(), ParameterError> {
    if text.is_empty() || text.len() > max_bytes || text.chars().any(char::is_control) {
        Err(ParameterError::InvalidText)
    } else {
        Ok(())
    }
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn hash_value(hasher: &mut Sha256, value: &ParameterValue) {
    match value {
        ParameterValue::Quantity { value } => {
            hasher.update([0]);
            hasher.update([match value.unit.quantity_kind() {
                QuantityKind::Length => 0,
                QuantityKind::Angle => 1,
                QuantityKind::Scalar => 2,
            }]);
            hasher.update(normalize_zero(value.magnitude).to_bits().to_be_bytes());
        }
        ParameterValue::Integer { value } => {
            hasher.update([1]);
            hasher.update(value.to_be_bytes());
        }
        ParameterValue::Boolean { value } => {
            hasher.update([2, u8::from(*value)]);
        }
        ParameterValue::Choice { value } => {
            hasher.update([3]);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
}

fn decode_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocated(value: u64) -> ParameterId {
        ParameterId::from_allocated(value)
    }

    fn length_spec(key: &str) -> ParameterSpec {
        ParameterSpec::new(key, key, ParameterType::Quantity(QuantityKind::Length))
            .with_display_unit(ParameterUnit::Millimeter)
    }

    #[test]
    fn units_evaluate_to_canonical_values_and_digests() {
        let id = allocated(1);
        let inch = ParameterTable::try_from_records(vec![ParameterRecord {
            id,
            spec: length_spec("Length"),
            binding: ParameterBinding::literal(ParameterValue::quantity(1.0, ParameterUnit::Inch)),
        }])
        .expect("inch table should validate");
        let millimetres = ParameterTable::try_from_records(vec![ParameterRecord {
            id,
            spec: length_spec("Length"),
            binding: ParameterBinding::literal(ParameterValue::quantity(
                25.4,
                ParameterUnit::Millimeter,
            )),
        }])
        .expect("millimetre table should validate");

        let inch_values = inch
            .evaluate(&ParameterOverrides::default())
            .expect("inch should evaluate");
        assert_eq!(
            inch_values.get(id),
            Some(&ParameterValue::quantity(25.4, ParameterUnit::Millimeter))
        );
        assert_eq!(
            inch_values.binding_digest(),
            millimetres
                .evaluate(&ParameterOverrides::default())
                .expect("millimetres should evaluate")
                .binding_digest()
        );
    }

    #[test]
    fn expression_dependencies_evaluate_independently_of_record_order() {
        let base = allocated(1);
        let doubled = allocated(2);
        let table = ParameterTable::try_from_records(vec![
            ParameterRecord {
                id: doubled,
                spec: length_spec("DoubleLength"),
                binding: ParameterBinding::expression(ParameterExpression::Multiply {
                    left: Box::new(ParameterExpression::reference(base)),
                    right: Box::new(ParameterExpression::literal(ParameterValue::quantity(
                        2.0,
                        ParameterUnit::Scalar,
                    ))),
                }),
            },
            ParameterRecord {
                id: base,
                spec: length_spec("Length"),
                binding: ParameterBinding::literal(ParameterValue::quantity(
                    3.0,
                    ParameterUnit::Centimeter,
                )),
            },
        ])
        .expect("expression table should validate");

        assert_eq!(
            table
                .evaluate(&ParameterOverrides::default())
                .expect("expression should evaluate")
                .get(doubled),
            Some(&ParameterValue::quantity(60.0, ParameterUnit::Millimeter))
        );
    }

    #[test]
    fn missing_required_input_is_valid_to_store_but_not_to_resolve() {
        let id = allocated(1);
        let mut spec = length_spec("Length");
        spec.metadata.exposure = ParameterExposure::UserInput;
        let table = ParameterTable::try_from_records(vec![ParameterRecord {
            id,
            spec,
            binding: ParameterBinding::Unresolved,
        }])
        .expect("required input is a valid authored state");

        assert!(table.records()[0].is_required_input());
        assert_eq!(
            table.evaluate(&ParameterOverrides::default()),
            Err(ParameterError::MissingValue(id))
        );
    }

    #[test]
    fn explicit_override_wins_over_default_and_binding() {
        let id = allocated(1);
        let mut spec = length_spec("Length");
        spec.metadata.default_value =
            Some(ParameterValue::quantity(10.0, ParameterUnit::Millimeter));
        let table = ParameterTable::try_from_records(vec![ParameterRecord {
            id,
            spec,
            binding: ParameterBinding::literal(ParameterValue::quantity(
                20.0,
                ParameterUnit::Millimeter,
            )),
        }])
        .expect("table should validate");
        let mut overrides = ParameterOverrides::default();
        overrides
            .set(
                id,
                ParameterValue::quantity(30.0, ParameterUnit::Millimeter),
            )
            .expect("override should validate");

        assert_eq!(
            table
                .evaluate(&overrides)
                .expect("override should resolve")
                .get(id),
            Some(&ParameterValue::quantity(30.0, ParameterUnit::Millimeter))
        );
    }

    #[test]
    fn cycles_and_unknown_references_fail_closed() {
        let first = allocated(1);
        let second = allocated(2);
        let cycle = ParameterTable::try_from_records(vec![
            ParameterRecord {
                id: first,
                spec: length_spec("A"),
                binding: ParameterBinding::expression(ParameterExpression::reference(second)),
            },
            ParameterRecord {
                id: second,
                spec: length_spec("B"),
                binding: ParameterBinding::expression(ParameterExpression::reference(first)),
            },
        ]);
        assert!(matches!(cycle, Err(ParameterError::DependencyCycle { .. })));

        let unknown = ParameterTable::try_from_records(vec![ParameterRecord {
            id: first,
            spec: length_spec("A"),
            binding: ParameterBinding::expression(ParameterExpression::reference(second)),
        }]);
        assert_eq!(unknown, Err(ParameterError::UnknownParameter(second)));
    }

    #[test]
    fn incompatible_dimensions_and_nonfinite_values_are_rejected() {
        let id = allocated(1);
        let invalid_expression = ParameterExpression::Add {
            left: Box::new(ParameterExpression::literal(ParameterValue::quantity(
                10.0,
                ParameterUnit::Millimeter,
            ))),
            right: Box::new(ParameterExpression::literal(ParameterValue::quantity(
                30.0,
                ParameterUnit::Degree,
            ))),
        };
        assert!(matches!(
            ParameterTable::try_from_records(vec![ParameterRecord {
                id,
                spec: length_spec("Length"),
                binding: ParameterBinding::expression(invalid_expression),
            }]),
            Err(ParameterError::IncompatibleOperands { .. })
        ));
        assert_eq!(
            ParameterTable::try_from_records(vec![ParameterRecord {
                id,
                spec: length_spec("Length"),
                binding: ParameterBinding::literal(ParameterValue::quantity(
                    f64::INFINITY,
                    ParameterUnit::Millimeter,
                )),
            }]),
            Err(ParameterError::NonFinite)
        );
    }

    #[test]
    fn ranges_choices_and_steps_are_validated() {
        let id = allocated(1);
        let mut ranged = length_spec("Length");
        ranged.metadata.minimum = Some(ParameterValue::quantity(1.0, ParameterUnit::Millimeter));
        ranged.metadata.maximum = Some(ParameterValue::quantity(2.0, ParameterUnit::Millimeter));
        ranged.metadata.step = Some(ParameterValue::quantity(0.1, ParameterUnit::Millimeter));
        assert_eq!(
            ParameterTable::try_from_records(vec![ParameterRecord {
                id,
                spec: ranged,
                binding: ParameterBinding::literal(ParameterValue::quantity(
                    3.0,
                    ParameterUnit::Millimeter,
                )),
            }]),
            Err(ParameterError::OutOfRange(id))
        );

        let mut choice_spec = ParameterSpec::new("Finish", "Finish", ParameterType::Choice);
        choice_spec.metadata.choices = vec![ParameterChoice::new("plain", "Plain")];
        assert_eq!(
            ParameterTable::try_from_records(vec![ParameterRecord {
                id,
                spec: choice_spec,
                binding: ParameterBinding::literal(ParameterValue::choice("anodized")),
            }]),
            Err(ParameterError::InvalidChoice {
                parameter: id,
                value: "anodized".to_owned(),
            })
        );
    }

    #[test]
    fn serde_rejects_a_tampered_table_and_digest_round_trips() {
        let id = allocated(1);
        let table = ParameterTable::try_from_records(vec![ParameterRecord {
            id,
            spec: length_spec("Length"),
            binding: ParameterBinding::literal(ParameterValue::quantity(
                20.0,
                ParameterUnit::Millimeter,
            )),
        }])
        .expect("table should validate");
        let mut json = serde_json::to_value(&table).expect("table should encode");
        json["parameters"][0]["id"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ParameterTable>(json).is_err());

        let digest = table
            .evaluate(&ParameterOverrides::default())
            .expect("table should evaluate")
            .binding_digest();
        let encoded = serde_json::to_string(&digest).expect("digest should encode");
        assert_eq!(
            serde_json::from_str::<ParameterBindingDigest>(&encoded).expect("digest should decode"),
            digest
        );
    }

    #[test]
    fn expression_depth_is_bounded() {
        let mut expression = ParameterExpression::literal(ParameterValue::integer(1));
        for _ in 0..=MAX_EXPRESSION_DEPTH {
            expression = ParameterExpression::Negate {
                operand: Box::new(expression),
            };
        }
        let id = allocated(1);
        assert_eq!(
            ParameterTable::try_from_records(vec![ParameterRecord {
                id,
                spec: ParameterSpec::new("Count", "Count", ParameterType::Integer),
                binding: ParameterBinding::expression(expression),
            }]),
            Err(ParameterError::ExpressionTooDeep)
        );
    }
}

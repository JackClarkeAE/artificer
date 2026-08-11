//! Immutable, content-addressed part definitions for the Artificer Part Library.
//!
//! This crate deliberately has no dependency on the model, kernel, renderer, or
//! UI crates. A catalog package carries a self-contained canonical JSON part
//! document plus the parameter interface needed to resolve a concrete variant.
//! Packages never contain an authoritative filesystem path: local paths belong
//! only to [`CatalogStore`].

mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub use store::{
    CatalogEntry, CatalogIndex, CatalogStore, IndexRebuildReport, RejectedCatalogEntry, SearchQuery,
};

/// Stable package-format marker written into every catalog object.
pub const PART_PACKAGE_FORMAT: &str = "artificer.catalog.part";
/// Catalog schema emitted by this crate.
pub const CURRENT_PACKAGE_VERSION: u32 = 1;
/// Maximum encoded size accepted for one catalog package.
pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum canonical JSON size accepted for an embedded native part document.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum exposed parameters on one part definition.
pub const MAX_PARAMETERS: usize = 256;
/// Maximum number of choices on one enumerated parameter.
pub const MAX_PARAMETER_CHOICES: usize = 256;
/// Maximum metadata tags on one definition.
pub const MAX_TAGS: usize = 128;
/// Maximum catalog entries considered in one index rebuild.
pub const MAX_INDEX_ENTRIES: usize = 100_000;
/// Maximum byte length of an identifier used in an internal catalog path.
pub const MAX_IDENTIFIER_BYTES: usize = 64;
/// Maximum byte length of a short user-visible field.
pub const MAX_SHORT_TEXT_BYTES: usize = 256;
/// Maximum byte length of a description.
pub const MAX_DESCRIPTION_BYTES: usize = 4_096;

/// Catalog validation, persistence, or integrity failure.
#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("invalid {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("{resource} exceeds its limit of {limit} bytes/items (actual {actual})")]
    ResourceLimit {
        resource: &'static str,
        limit: usize,
        actual: usize,
    },
    #[error("unsupported catalog format `{0}`")]
    UnsupportedFormat(String),
    #[error("unsupported catalog schema version {0}")]
    UnsupportedVersion(u32),
    #[error("catalog content digest mismatch: expected {expected}, computed {actual}")]
    DigestMismatch {
        expected: ContentDigest,
        actual: ContentDigest,
    },
    #[error(
        "part {definition} revision {revision} is already published as {existing}; refusing replacement with {attempted}"
    )]
    RevisionConflict {
        definition: PartDefinitionId,
        revision: PartRevision,
        existing: ContentDigest,
        attempted: ContentDigest,
    },
    #[error("catalog object {0} was not found")]
    ObjectNotFound(ContentDigest),
    #[error("part {definition} revision {revision} was not found")]
    RevisionNotFound {
        definition: PartDefinitionId,
        revision: PartRevision,
    },
    #[error("unsafe or unexpected catalog filesystem entry: {0}")]
    UnsafeFilesystemEntry(String),
    #[error("catalog lock was poisoned")]
    LockPoisoned,
    #[error("catalog JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("catalog I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn invalid(field: &'static str, reason: impl Into<String>) -> CatalogError {
    CatalogError::InvalidField {
        field,
        reason: reason.into(),
    }
}

fn validate_text(
    value: &str,
    field: &'static str,
    maximum_bytes: usize,
    allow_empty: bool,
) -> Result<(), CatalogError> {
    if !allow_empty && value.trim().is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.len() > maximum_bytes {
        return Err(CatalogError::ResourceLimit {
            resource: field,
            limit: maximum_bytes,
            actual: value.len(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

fn validate_path_identifier(value: &str, field: &'static str) -> Result<(), CatalogError> {
    validate_text(value, field, MAX_IDENTIFIER_BYTES, false)?;
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return Err(invalid(field, "must not be empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(invalid(field, "must begin with an ASCII letter or digit"));
    }
    if !characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(invalid(
            field,
            "may contain only ASCII letters, digits, '.', '-' and '_'",
        ));
    }
    if value == "." || value == ".." || value.contains("..") {
        return Err(invalid(field, "must not contain a parent-path component"));
    }
    Ok(())
}

macro_rules! validated_identifier {
    ($name:ident, $field:literal) => {
        #[doc = concat!("Validated, path-safe `", stringify!($name), "`.")]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, CatalogError> {
                let value = value.into();
                validate_path_identifier(&value, $field)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = CatalogError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(de::Error::custom)
            }
        }
    };
}

validated_identifier!(PartDefinitionId, "part definition ID");
validated_identifier!(ParameterId, "parameter ID");

/// Immutable authored revision of one part definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartRevision {
    major: u32,
    minor: u32,
    patch: u32,
}

impl PartRevision {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    #[must_use]
    pub const fn patch(self) -> u32 {
        self.patch
    }
}

impl fmt::Display for PartRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for PartRevision {
    type Err = CatalogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut components = value.split('.');
        let parse = |component: Option<&str>| {
            component
                .ok_or_else(|| invalid("part revision", "expected MAJOR.MINOR.PATCH"))?
                .parse::<u32>()
                .map_err(|_| invalid("part revision", "expected unsigned decimal components"))
        };
        let revision = Self::new(
            parse(components.next())?,
            parse(components.next())?,
            parse(components.next())?,
        );
        if components.next().is_some() || revision.to_string() != value {
            return Err(invalid(
                "part revision",
                "must use canonical MAJOR.MINOR.PATCH form",
            ));
        }
        Ok(revision)
    }
}

/// SHA-256 digest used as the immutable package address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
        }
        output
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for ContentDigest {
    type Err = CatalogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid(
                "content digest",
                "must contain exactly 64 hexadecimal characters",
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(pair)
                .map_err(|_| invalid("content digest", "must be ASCII hexadecimal"))?;
            bytes[index] = u8::from_str_radix(pair, 16)
                .map_err(|_| invalid("content digest", "must be hexadecimal"))?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for ContentDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(de::Error::custom)
    }
}

/// A finite canonical real value. Negative zero is normalized to positive zero.
#[derive(Clone, Copy, Debug)]
pub struct FiniteReal(f64);

impl FiniteReal {
    pub fn new(value: f64) -> Result<Self, CatalogError> {
        if !value.is_finite() {
            return Err(invalid("parameter value", "must be finite"));
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for FiniteReal {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for FiniteReal {}

impl PartialOrd for FiniteReal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FiniteReal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for FiniteReal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Serialize for FiniteReal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for FiniteReal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Quantity represented by a real-valued parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealQuantity {
    Length,
    Angle,
    Scalar,
}

/// UI display unit. Real values remain stored in canonical model units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayUnit {
    Millimetre,
    Centimetre,
    Metre,
    Inch,
    Radian,
    Degree,
    Unitless,
}

impl DisplayUnit {
    const fn supports(self, quantity: RealQuantity) -> bool {
        matches!(
            (self, quantity),
            (
                Self::Millimetre | Self::Centimetre | Self::Metre | Self::Inch,
                RealQuantity::Length
            ) | (Self::Radian | Self::Degree, RealQuantity::Angle)
                | (Self::Unitless, RealQuantity::Scalar)
        )
    }
}

/// Inclusive numeric bounds and positive increment for one real parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealRules {
    minimum: Option<FiniteReal>,
    maximum: Option<FiniteReal>,
    step: Option<FiniteReal>,
}

impl RealRules {
    pub fn new(
        minimum: Option<f64>,
        maximum: Option<f64>,
        step: Option<f64>,
    ) -> Result<Self, CatalogError> {
        let rules = Self {
            minimum: minimum.map(FiniteReal::new).transpose()?,
            maximum: maximum.map(FiniteReal::new).transpose()?,
            step: step.map(FiniteReal::new).transpose()?,
        };
        rules.validate()?;
        Ok(rules)
    }

    #[must_use]
    pub const fn unconstrained() -> Self {
        Self {
            minimum: None,
            maximum: None,
            step: None,
        }
    }

    #[must_use]
    pub const fn minimum(&self) -> Option<FiniteReal> {
        self.minimum
    }

    #[must_use]
    pub const fn maximum(&self) -> Option<FiniteReal> {
        self.maximum
    }

    #[must_use]
    pub const fn step(&self) -> Option<FiniteReal> {
        self.step
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if let (Some(minimum), Some(maximum)) = (self.minimum, self.maximum)
            && minimum > maximum
        {
            return Err(invalid(
                "real parameter range",
                "minimum must not exceed maximum",
            ));
        }
        if self.step.is_some_and(|step| step.get() <= 0.0) {
            return Err(invalid("real parameter step", "must be positive"));
        }
        Ok(())
    }

    fn contains(&self, value: FiniteReal) -> bool {
        self.minimum.is_none_or(|minimum| value >= minimum)
            && self.maximum.is_none_or(|maximum| value <= maximum)
    }
}

/// Inclusive bounds and positive increment for an integer parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegerRules {
    minimum: Option<i64>,
    maximum: Option<i64>,
    step: Option<u64>,
}

impl IntegerRules {
    pub fn new(
        minimum: Option<i64>,
        maximum: Option<i64>,
        step: Option<u64>,
    ) -> Result<Self, CatalogError> {
        let rules = Self {
            minimum,
            maximum,
            step,
        };
        rules.validate()?;
        Ok(rules)
    }

    #[must_use]
    pub const fn unconstrained() -> Self {
        Self {
            minimum: None,
            maximum: None,
            step: None,
        }
    }

    #[must_use]
    pub const fn minimum(&self) -> Option<i64> {
        self.minimum
    }

    #[must_use]
    pub const fn maximum(&self) -> Option<i64> {
        self.maximum
    }

    #[must_use]
    pub const fn step(&self) -> Option<u64> {
        self.step
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if let (Some(minimum), Some(maximum)) = (self.minimum, self.maximum)
            && minimum > maximum
        {
            return Err(invalid(
                "integer parameter range",
                "minimum must not exceed maximum",
            ));
        }
        if self.step == Some(0) {
            return Err(invalid("integer parameter step", "must be positive"));
        }
        Ok(())
    }

    fn contains(&self, value: i64) -> bool {
        self.minimum.is_none_or(|minimum| value >= minimum)
            && self.maximum.is_none_or(|maximum| value <= maximum)
    }
}

/// Typed public interface of one exposed part parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParameterDomain {
    Real {
        quantity: RealQuantity,
        display_unit: DisplayUnit,
        default: Option<FiniteReal>,
        rules: RealRules,
    },
    Integer {
        default: Option<i64>,
        rules: IntegerRules,
    },
    Boolean {
        default: Option<bool>,
    },
    Choice {
        default: Option<String>,
        options: Vec<String>,
    },
}

impl ParameterDomain {
    fn validate(&self) -> Result<(), CatalogError> {
        match self {
            Self::Real {
                quantity,
                display_unit,
                default,
                rules,
            } => {
                if !display_unit.supports(*quantity) {
                    return Err(invalid(
                        "parameter display unit",
                        "is dimensionally incompatible with its quantity",
                    ));
                }
                rules.validate()?;
                if default.is_some_and(|value| !rules.contains(value)) {
                    return Err(invalid(
                        "real parameter default",
                        "must lie within the declared range",
                    ));
                }
            }
            Self::Integer { default, rules } => {
                rules.validate()?;
                if default.is_some_and(|value| !rules.contains(value)) {
                    return Err(invalid(
                        "integer parameter default",
                        "must lie within the declared range",
                    ));
                }
            }
            Self::Boolean { .. } => {}
            Self::Choice { default, options } => {
                if options.is_empty() {
                    return Err(invalid("parameter choices", "must not be empty"));
                }
                if options.len() > MAX_PARAMETER_CHOICES {
                    return Err(CatalogError::ResourceLimit {
                        resource: "parameter choices",
                        limit: MAX_PARAMETER_CHOICES,
                        actual: options.len(),
                    });
                }
                let mut unique = BTreeSet::new();
                for option in options {
                    validate_text(option, "parameter choice", MAX_SHORT_TEXT_BYTES, false)?;
                    if !unique.insert(option) {
                        return Err(invalid("parameter choices", "must be unique"));
                    }
                }
                if default
                    .as_ref()
                    .is_some_and(|value| !unique.contains(value))
                {
                    return Err(invalid(
                        "choice parameter default",
                        "must be one of the declared choices",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether insertion must supply a value because no default exists.
    #[must_use]
    pub const fn requires_input(&self) -> bool {
        match self {
            Self::Real { default, .. } => default.is_none(),
            Self::Integer { default, .. } => default.is_none(),
            Self::Boolean { default } => default.is_none(),
            Self::Choice { default, .. } => default.is_none(),
        }
    }
}

/// One exposed, ordered parameter shown by the Part Library UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterSpec {
    id: ParameterId,
    label: String,
    description: Option<String>,
    group: Option<String>,
    order: u32,
    domain: ParameterDomain,
}

impl ParameterSpec {
    pub fn real(
        id: ParameterId,
        label: impl Into<String>,
        order: u32,
        quantity: RealQuantity,
        display_unit: DisplayUnit,
        default: Option<f64>,
        rules: RealRules,
    ) -> Result<Self, CatalogError> {
        Self::new(
            id,
            label,
            order,
            ParameterDomain::Real {
                quantity,
                display_unit,
                default: default.map(FiniteReal::new).transpose()?,
                rules,
            },
        )
    }

    pub fn integer(
        id: ParameterId,
        label: impl Into<String>,
        order: u32,
        default: Option<i64>,
        rules: IntegerRules,
    ) -> Result<Self, CatalogError> {
        Self::new(
            id,
            label,
            order,
            ParameterDomain::Integer { default, rules },
        )
    }

    pub fn boolean(
        id: ParameterId,
        label: impl Into<String>,
        order: u32,
        default: Option<bool>,
    ) -> Result<Self, CatalogError> {
        Self::new(id, label, order, ParameterDomain::Boolean { default })
    }

    pub fn choice(
        id: ParameterId,
        label: impl Into<String>,
        order: u32,
        default: Option<String>,
        options: Vec<String>,
    ) -> Result<Self, CatalogError> {
        Self::new(
            id,
            label,
            order,
            ParameterDomain::Choice { default, options },
        )
    }

    fn new(
        id: ParameterId,
        label: impl Into<String>,
        order: u32,
        domain: ParameterDomain,
    ) -> Result<Self, CatalogError> {
        let spec = Self {
            id,
            label: label.into(),
            description: None,
            group: None,
            order,
            domain,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn with_description(
        mut self,
        description: impl Into<String>,
    ) -> Result<Self, CatalogError> {
        self.description = Some(description.into());
        self.validate()?;
        Ok(self)
    }

    pub fn with_group(mut self, group: impl Into<String>) -> Result<Self, CatalogError> {
        self.group = Some(group.into());
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    pub const fn id(&self) -> &ParameterId {
        &self.id
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    #[must_use]
    pub const fn order(&self) -> u32 {
        self.order
    }

    #[must_use]
    pub const fn domain(&self) -> &ParameterDomain {
        &self.domain
    }

    #[must_use]
    pub const fn requires_input(&self) -> bool {
        self.domain.requires_input()
    }

    fn validate(&self) -> Result<(), CatalogError> {
        validate_text(&self.label, "parameter label", MAX_SHORT_TEXT_BYTES, false)?;
        if let Some(description) = &self.description {
            validate_text(
                description,
                "parameter description",
                MAX_DESCRIPTION_BYTES,
                true,
            )?;
        }
        if let Some(group) = &self.group {
            validate_text(group, "parameter group", MAX_SHORT_TEXT_BYTES, false)?;
        }
        self.domain.validate()
    }
}

/// Searchable, immutable authored metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartMetadata {
    name: String,
    description: Option<String>,
    category: Option<String>,
    tags: BTreeSet<String>,
    material: Option<String>,
    part_number: Option<String>,
}

impl PartMetadata {
    pub fn new(name: impl Into<String>) -> Result<Self, CatalogError> {
        let metadata = Self {
            name: name.into(),
            description: None,
            category: None,
            tags: BTreeSet::new(),
            material: None,
            part_number: None,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn with_description(mut self, value: impl Into<String>) -> Result<Self, CatalogError> {
        self.description = Some(value.into());
        self.validate()?;
        Ok(self)
    }

    pub fn with_category(mut self, value: impl Into<String>) -> Result<Self, CatalogError> {
        self.category = Some(value.into());
        self.validate()?;
        Ok(self)
    }

    pub fn with_material(mut self, value: impl Into<String>) -> Result<Self, CatalogError> {
        self.material = Some(value.into());
        self.validate()?;
        Ok(self)
    }

    pub fn with_part_number(mut self, value: impl Into<String>) -> Result<Self, CatalogError> {
        self.part_number = Some(value.into());
        self.validate()?;
        Ok(self)
    }

    pub fn with_tags<I, S>(mut self, values: I) -> Result<Self, CatalogError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = values.into_iter().map(Into::into).collect();
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    #[must_use]
    pub const fn tags(&self) -> &BTreeSet<String> {
        &self.tags
    }

    #[must_use]
    pub fn material(&self) -> Option<&str> {
        self.material.as_deref()
    }

    #[must_use]
    pub fn part_number(&self) -> Option<&str> {
        self.part_number.as_deref()
    }

    fn validate(&self) -> Result<(), CatalogError> {
        validate_text(&self.name, "part name", MAX_SHORT_TEXT_BYTES, false)?;
        if let Some(description) = &self.description {
            validate_text(description, "part description", MAX_DESCRIPTION_BYTES, true)?;
        }
        for (field, value) in [
            ("part category", self.category.as_deref()),
            ("part material", self.material.as_deref()),
            ("part number", self.part_number.as_deref()),
        ] {
            if let Some(value) = value {
                validate_text(value, field, MAX_SHORT_TEXT_BYTES, false)?;
            }
        }
        if self.tags.len() > MAX_TAGS {
            return Err(CatalogError::ResourceLimit {
                resource: "part tags",
                limit: MAX_TAGS,
                actual: self.tags.len(),
            });
        }
        for tag in &self.tags {
            validate_text(tag, "part tag", MAX_SHORT_TEXT_BYTES, false)?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum CanonicalJson {
    Null,
    Boolean(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

impl From<serde_json::Value> for CanonicalJson {
    fn from(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(value) => Self::Boolean(value),
            serde_json::Value::Number(value) => Self::Number(value),
            serde_json::Value::String(value) => Self::String(value),
            serde_json::Value::Array(values) => {
                Self::Array(values.into_iter().map(Self::from).collect())
            }
            serde_json::Value::Object(values) => Self::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
            ),
        }
    }
}

/// Self-contained, canonical JSON recipe used to regenerate a native part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedDocument {
    media_type: String,
    schema_version: u32,
    canonical_json: String,
}

impl EmbeddedDocument {
    pub fn from_json(
        media_type: impl Into<String>,
        schema_version: u32,
        bytes: impl AsRef<[u8]>,
    ) -> Result<Self, CatalogError> {
        let bytes = bytes.as_ref();
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(CatalogError::ResourceLimit {
                resource: "embedded document",
                limit: MAX_DOCUMENT_BYTES,
                actual: bytes.len(),
            });
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let canonical_json = serde_json::to_string(&CanonicalJson::from(value))?;
        let document = Self {
            media_type: media_type.into(),
            schema_version,
            canonical_json,
        };
        document.validate()?;
        Ok(document)
    }

    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    fn validate(&self) -> Result<(), CatalogError> {
        validate_text(
            &self.media_type,
            "document media type",
            MAX_SHORT_TEXT_BYTES,
            false,
        )?;
        if self.schema_version == 0 {
            return Err(invalid(
                "document schema version",
                "must be greater than zero",
            ));
        }
        if self.canonical_json.len() > MAX_DOCUMENT_BYTES {
            return Err(CatalogError::ResourceLimit {
                resource: "embedded document",
                limit: MAX_DOCUMENT_BYTES,
                actual: self.canonical_json.len(),
            });
        }
        let value: serde_json::Value = serde_json::from_str(&self.canonical_json)?;
        let canonical = serde_json::to_string(&CanonicalJson::from(value))?;
        if canonical != self.canonical_json {
            return Err(invalid(
                "embedded document",
                "JSON must use the canonical encoding",
            ));
        }
        Ok(())
    }
}

/// Whether a published definition has an exposed parameter interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Fixed,
    Parametric,
}

/// Immutable authored recipe, metadata, and exposed parameter contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartDefinition {
    id: PartDefinitionId,
    revision: PartRevision,
    kind: PartKind,
    metadata: PartMetadata,
    parameters: Vec<ParameterSpec>,
    document: EmbeddedDocument,
}

impl PartDefinition {
    pub fn fixed(
        id: PartDefinitionId,
        revision: PartRevision,
        metadata: PartMetadata,
        document: EmbeddedDocument,
    ) -> Result<Self, CatalogError> {
        Self::new(
            id,
            revision,
            PartKind::Fixed,
            metadata,
            Vec::new(),
            document,
        )
    }

    pub fn parametric(
        id: PartDefinitionId,
        revision: PartRevision,
        metadata: PartMetadata,
        parameters: Vec<ParameterSpec>,
        document: EmbeddedDocument,
    ) -> Result<Self, CatalogError> {
        Self::new(
            id,
            revision,
            PartKind::Parametric,
            metadata,
            parameters,
            document,
        )
    }

    fn new(
        id: PartDefinitionId,
        revision: PartRevision,
        kind: PartKind,
        metadata: PartMetadata,
        mut parameters: Vec<ParameterSpec>,
        document: EmbeddedDocument,
    ) -> Result<Self, CatalogError> {
        parameters.sort_by(|left, right| {
            left.order()
                .cmp(&right.order())
                .then_with(|| left.id().cmp(right.id()))
        });
        let definition = Self {
            id,
            revision,
            kind,
            metadata,
            parameters,
            document,
        };
        definition.validate()?;
        Ok(definition)
    }

    #[must_use]
    pub const fn id(&self) -> &PartDefinitionId {
        &self.id
    }

    #[must_use]
    pub const fn revision(&self) -> PartRevision {
        self.revision
    }

    #[must_use]
    pub const fn kind(&self) -> PartKind {
        self.kind
    }

    #[must_use]
    pub const fn metadata(&self) -> &PartMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn parameters(&self) -> &[ParameterSpec] {
        &self.parameters
    }

    #[must_use]
    pub const fn document(&self) -> &EmbeddedDocument {
        &self.document
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        self.metadata.validate()?;
        self.document.validate()?;
        if self.parameters.len() > MAX_PARAMETERS {
            return Err(CatalogError::ResourceLimit {
                resource: "part parameters",
                limit: MAX_PARAMETERS,
                actual: self.parameters.len(),
            });
        }
        match (self.kind, self.parameters.is_empty()) {
            (PartKind::Fixed, false) => {
                return Err(invalid(
                    "fixed part definition",
                    "must not expose parameters",
                ));
            }
            (PartKind::Parametric, true) => {
                return Err(invalid(
                    "parametric part definition",
                    "must expose at least one parameter",
                ));
            }
            _ => {}
        }
        let mut ids = BTreeSet::new();
        for parameter in &self.parameters {
            parameter.validate()?;
            if !ids.insert(parameter.id()) {
                return Err(invalid("part parameters", "parameter IDs must be unique"));
            }
        }
        if self
            .parameters
            .windows(2)
            .any(|pair| (pair[0].order(), pair[0].id()) > (pair[1].order(), pair[1].id()))
        {
            return Err(invalid(
                "part parameters",
                "must use canonical order then ID ordering",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct DigestMaterial<'a> {
    format: &'static str,
    schema_version: u32,
    definition: &'a PartDefinition,
}

fn compute_digest(definition: &PartDefinition) -> Result<ContentDigest, CatalogError> {
    let material = DigestMaterial {
        format: PART_PACKAGE_FORMAT,
        schema_version: CURRENT_PACKAGE_VERSION,
        definition,
    };
    let bytes = serde_json::to_vec(&material)?;
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Ok(ContentDigest(digest))
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PartPackageRef<'a> {
    format: &'static str,
    schema_version: u32,
    content_digest: ContentDigest,
    definition: &'a PartDefinition,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartPackageWire {
    format: String,
    schema_version: u32,
    content_digest: ContentDigest,
    definition: PartDefinition,
}

/// Sealed, immutable catalog object addressed by its verified content digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartPackage {
    content_digest: ContentDigest,
    definition: PartDefinition,
}

impl PartPackage {
    pub fn seal(definition: PartDefinition) -> Result<Self, CatalogError> {
        definition.validate()?;
        let content_digest = compute_digest(&definition)?;
        Ok(Self {
            content_digest,
            definition,
        })
    }

    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.content_digest
    }

    #[must_use]
    pub const fn definition(&self) -> &PartDefinition {
        &self.definition
    }

    pub fn verify(&self) -> Result<(), CatalogError> {
        self.definition.validate()?;
        let actual = compute_digest(&self.definition)?;
        if actual != self.content_digest {
            return Err(CatalogError::DigestMismatch {
                expected: self.content_digest,
                actual,
            });
        }
        Ok(())
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, CatalogError> {
        self.verify()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(CatalogError::ResourceLimit {
                resource: "catalog package",
                limit: MAX_PACKAGE_BYTES,
                actual: bytes.len(),
            });
        }
        Ok(bytes)
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, CatalogError> {
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(CatalogError::ResourceLimit {
                resource: "catalog package",
                limit: MAX_PACKAGE_BYTES,
                actual: bytes.len(),
            });
        }
        let wire = serde_json::from_slice(bytes)?;
        Self::from_wire(wire)
    }

    fn from_wire(wire: PartPackageWire) -> Result<Self, CatalogError> {
        if wire.format != PART_PACKAGE_FORMAT {
            return Err(CatalogError::UnsupportedFormat(wire.format));
        }
        if wire.schema_version != CURRENT_PACKAGE_VERSION {
            return Err(CatalogError::UnsupportedVersion(wire.schema_version));
        }
        let package = Self {
            content_digest: wire.content_digest,
            definition: wire.definition,
        };
        package.verify()?;
        Ok(package)
    }
}

impl Serialize for PartPackage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        PartPackageRef {
            format: PART_PACKAGE_FORMAT,
            schema_version: CURRENT_PACKAGE_VERSION,
            content_digest: self.content_digest,
            definition: &self.definition,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PartPackage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PartPackageWire::deserialize(deserializer)?;
        Self::from_wire(wire).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(bytes: &[u8]) -> EmbeddedDocument {
        EmbeddedDocument::from_json("application/vnd.artificer.native+json", 2, bytes).unwrap()
    }

    fn extrusion_definition(tags: &[&str], json: &[u8]) -> PartDefinition {
        let metadata = PartMetadata::new("20 × 20 Aluminium Extrusion")
            .unwrap()
            .with_category("Structural / Extrusion")
            .unwrap()
            .with_material("6063-T6 Aluminium")
            .unwrap()
            .with_tags(tags.iter().copied())
            .unwrap();
        let length = ParameterSpec::real(
            ParameterId::parse("length").unwrap(),
            "Length",
            0,
            RealQuantity::Length,
            DisplayUnit::Millimetre,
            None,
            RealRules::new(Some(1.0), Some(6_000.0), Some(1.0)).unwrap(),
        )
        .unwrap();
        PartDefinition::parametric(
            PartDefinitionId::parse("profile-2020").unwrap(),
            PartRevision::new(1, 0, 0),
            metadata,
            vec![length],
            document(json),
        )
        .unwrap()
    }

    #[test]
    fn canonical_json_and_sets_make_package_bytes_deterministic() {
        let left = PartPackage::seal(extrusion_definition(
            &["aluminium", "metric"],
            br#"{ "features": [1, 2], "name": "profile" }"#,
        ))
        .unwrap();
        let right = PartPackage::seal(extrusion_definition(
            &["metric", "aluminium"],
            br#"{"name":"profile","features":[1,2]}"#,
        ))
        .unwrap();

        assert_eq!(left.content_digest(), right.content_digest());
        assert_eq!(
            left.to_json_bytes().unwrap(),
            right.to_json_bytes().unwrap()
        );
        let decoded = PartPackage::from_json_bytes(&left.to_json_bytes().unwrap()).unwrap();
        assert_eq!(decoded, left);
    }

    #[test]
    fn required_input_is_derived_from_absent_default() {
        let definition = extrusion_definition(&[], br#"{"features":[]}"#);
        assert_eq!(definition.kind(), PartKind::Parametric);
        assert!(definition.parameters()[0].requires_input());

        let with_default = ParameterSpec::real(
            ParameterId::parse("length").unwrap(),
            "Length",
            0,
            RealQuantity::Length,
            DisplayUnit::Millimetre,
            Some(500.0),
            RealRules::new(Some(1.0), None, None).unwrap(),
        )
        .unwrap();
        assert!(!with_default.requires_input());
    }

    #[test]
    fn parameter_contract_rejects_invalid_ranges_units_and_choices() {
        assert!(RealRules::new(Some(10.0), Some(1.0), None).is_err());
        assert!(RealRules::new(None, None, Some(0.0)).is_err());
        assert!(
            ParameterSpec::real(
                ParameterId::parse("angle").unwrap(),
                "Angle",
                0,
                RealQuantity::Angle,
                DisplayUnit::Millimetre,
                None,
                RealRules::unconstrained(),
            )
            .is_err()
        );
        assert!(
            ParameterSpec::choice(
                ParameterId::parse("finish").unwrap(),
                "Finish",
                0,
                Some("black".into()),
                vec!["silver".into()],
            )
            .is_err()
        );
    }

    #[test]
    fn identifiers_cannot_escape_catalog_layout() {
        for identifier in [
            "../part",
            "part/name",
            ".hidden",
            "part..child",
            "part name",
        ] {
            assert!(PartDefinitionId::parse(identifier).is_err(), "{identifier}");
        }
        assert!(PartDefinitionId::parse("company.profile-2020_v2").is_ok());
    }

    #[test]
    fn oversized_embedded_document_is_rejected_before_parsing() {
        let oversized = vec![b' '; MAX_DOCUMENT_BYTES + 1];
        assert!(matches!(
            EmbeddedDocument::from_json("application/json", 1, oversized),
            Err(CatalogError::ResourceLimit {
                resource: "embedded document",
                ..
            })
        ));
    }

    #[test]
    fn digest_tampering_is_rejected_during_deserialization() {
        let package = PartPackage::seal(extrusion_definition(&[], br#"{"features":[]}"#)).unwrap();
        let mut value = serde_json::to_value(&package).unwrap();
        value["definition"]["metadata"]["name"] = "Tampered".into();
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            PartPackage::from_json_bytes(&bytes),
            Err(CatalogError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn revision_parser_requires_canonical_form() {
        assert_eq!(
            "1.2.3".parse::<PartRevision>().unwrap(),
            PartRevision::new(1, 2, 3)
        );
        for invalid in ["1.2", "1.2.3.4", "01.2.3", "v1.2.3"] {
            assert!(invalid.parse::<PartRevision>().is_err());
        }
    }
}

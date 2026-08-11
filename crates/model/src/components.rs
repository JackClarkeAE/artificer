//! Persistent, rigid occurrences of immutable part-definition revisions.
//!
//! A component instance owns no executable catalog code and no display-only
//! scale. It pins an immutable definition digest, a canonical resolved
//! parameter assignment, and one rigid pose. Geometry remains owned by the
//! body outputs of the feature which created the occurrence.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{BodyId, ComponentInstanceId, EvaluatedParameters, FeatureId, ParameterBindingDigest};

/// Maximum number of component occurrences in one document.
pub const MAX_COMPONENT_INSTANCES: usize = 4_096;
/// Maximum byte length of a catalog definition key.
pub const MAX_DEFINITION_KEY_BYTES: usize = 64;
/// Maximum absolute translation, in canonical model millimetres.
pub const MAX_COMPONENT_TRANSLATION: f64 = 1.0e12;
const UNIT_QUATERNION_TOLERANCE: f64 = 1.0e-12;

/// Semantic revision of an immutable part definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDefinitionRevision {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ComponentDefinitionRevision {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for ComponentDefinitionRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for ComponentDefinitionRevision {
    type Err = ComponentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut components = value.split('.');
        let parse = |component: Option<&str>| {
            component
                .ok_or(ComponentError::InvalidRevision)?
                .parse::<u32>()
                .map_err(|_| ComponentError::InvalidRevision)
        };
        let revision = Self::new(
            parse(components.next())?,
            parse(components.next())?,
            parse(components.next())?,
        );
        if components.next().is_some() || revision.to_string() != value {
            return Err(ComponentError::InvalidRevision);
        }
        Ok(revision)
    }
}

/// SHA-256 address of one immutable catalog package.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentContentDigest([u8; 32]);

impl ComponentContentDigest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ComponentContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for ComponentContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ComponentContentDigest {
    type Err = ComponentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ComponentError::InvalidContentDigest);
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let pair =
                std::str::from_utf8(pair).map_err(|_| ComponentError::InvalidContentDigest)?;
            bytes[index] =
                u8::from_str_radix(pair, 16).map_err(|_| ComponentError::InvalidContentDigest)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for ComponentContentDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ComponentContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// Exact immutable catalog revision pinned by a component occurrence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComponentDefinitionRef {
    definition_key: String,
    revision: ComponentDefinitionRevision,
    content_digest: ComponentContentDigest,
}

#[derive(Deserialize)]
struct RawComponentDefinitionRef {
    definition_key: String,
    revision: ComponentDefinitionRevision,
    content_digest: ComponentContentDigest,
}

impl<'de> Deserialize<'de> for ComponentDefinitionRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawComponentDefinitionRef::deserialize(deserializer)?;
        Self::new(raw.definition_key, raw.revision, raw.content_digest).map_err(de::Error::custom)
    }
}

impl ComponentDefinitionRef {
    pub fn new(
        definition_key: impl Into<String>,
        revision: ComponentDefinitionRevision,
        content_digest: ComponentContentDigest,
    ) -> Result<Self, ComponentError> {
        let definition_key = definition_key.into();
        validate_definition_key(&definition_key)?;
        Ok(Self {
            definition_key,
            revision,
            content_digest,
        })
    }

    #[must_use]
    pub fn definition_key(&self) -> &str {
        &self.definition_key
    }

    #[must_use]
    pub const fn revision(&self) -> ComponentDefinitionRevision {
        self.revision
    }

    #[must_use]
    pub const fn content_digest(&self) -> ComponentContentDigest {
        self.content_digest
    }
}

/// Finite translation in canonical model millimetres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ComponentTranslation {
    x: f64,
    y: f64,
    z: f64,
}

impl ComponentTranslation {
    pub fn new(x: f64, y: f64, z: f64) -> Result<Self, ComponentError> {
        let translation = Self {
            x: normalize_zero(x),
            y: normalize_zero(y),
            z: normalize_zero(z),
        };
        translation.validate()?;
        Ok(translation)
    }

    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }

    #[must_use]
    pub const fn z(self) -> f64 {
        self.z
    }

    fn validate(self) -> Result<(), ComponentError> {
        if [self.x, self.y, self.z]
            .into_iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_COMPONENT_TRANSLATION)
        {
            Ok(())
        } else {
            Err(ComponentError::InvalidTranslation)
        }
    }
}

impl<'de> Deserialize<'de> for ComponentTranslation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            x: f64,
            y: f64,
            z: f64,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.x, raw.y, raw.z).map_err(de::Error::custom)
    }
}

/// Unit, sign-canonical quaternion stored in `(w, x, y, z)` order.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CanonicalQuaternion {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}

impl CanonicalQuaternion {
    /// Normalizes a finite non-zero quaternion and selects one deterministic
    /// sign from the equivalent `q`/`-q` pair.
    pub fn new(w: f64, x: f64, y: f64, z: f64) -> Result<Self, ComponentError> {
        if ![w, x, y, z].into_iter().all(f64::is_finite) {
            return Err(ComponentError::InvalidQuaternion);
        }
        let norm = w.hypot(x).hypot(y).hypot(z);
        if !norm.is_finite() || norm <= f64::EPSILON {
            return Err(ComponentError::InvalidQuaternion);
        }
        let mut values = [w / norm, x / norm, y / norm, z / norm];
        if first_nonzero_is_negative(values) {
            for value in &mut values {
                *value = -*value;
            }
        }
        for value in &mut values {
            *value = normalize_zero(*value);
        }
        Ok(Self {
            w: values[0],
            x: values[1],
            y: values[2],
            z: values[3],
        })
    }

    #[must_use]
    pub const fn identity() -> Self {
        Self {
            w: 1.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }
    }

    #[must_use]
    pub const fn w(self) -> f64 {
        self.w
    }

    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }

    #[must_use]
    pub const fn z(self) -> f64 {
        self.z
    }

    fn validate_canonical(self) -> Result<(), ComponentError> {
        if ![self.w, self.x, self.y, self.z]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err(ComponentError::InvalidQuaternion);
        }
        let norm_squared = self.w.mul_add(
            self.w,
            self.x
                .mul_add(self.x, self.y.mul_add(self.y, self.z * self.z)),
        );
        if (norm_squared - 1.0).abs() > UNIT_QUATERNION_TOLERANCE
            || first_nonzero_is_negative([self.w, self.x, self.y, self.z])
        {
            return Err(ComponentError::NonCanonicalQuaternion);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CanonicalQuaternion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            w: f64,
            x: f64,
            y: f64,
            z: f64,
        }
        let raw = Raw::deserialize(deserializer)?;
        let quaternion = Self {
            w: normalize_zero(raw.w),
            x: normalize_zero(raw.x),
            y: normalize_zero(raw.y),
            z: normalize_zero(raw.z),
        };
        quaternion.validate_canonical().map_err(de::Error::custom)?;
        Ok(quaternion)
    }
}

/// Rigid occurrence pose. The type intentionally has no scale component.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RigidComponentPose {
    pub translation: ComponentTranslation,
    pub rotation: CanonicalQuaternion,
}

impl RigidComponentPose {
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            translation: ComponentTranslation {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            rotation: CanonicalQuaternion::identity(),
        }
    }

    #[must_use]
    pub const fn new(translation: ComponentTranslation, rotation: CanonicalQuaternion) -> Self {
        Self {
            translation,
            rotation,
        }
    }

    pub(crate) fn validate(self) -> Result<(), ComponentError> {
        self.translation.validate()?;
        self.rotation.validate_canonical()
    }
}

/// Staged component data supplied alongside a feature that creates its body
/// outputs. The binding digest is derived, never trusted as input.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentInstanceDraft {
    pub label: String,
    pub definition: ComponentDefinitionRef,
    pub resolved_parameters: EvaluatedParameters,
    pub pose: RigidComponentPose,
    pub visible: bool,
    pub suppressed: bool,
    pub grounded: bool,
}

impl ComponentInstanceDraft {
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        definition: ComponentDefinitionRef,
        resolved_parameters: EvaluatedParameters,
        pose: RigidComponentPose,
    ) -> Self {
        Self {
            label: label.into(),
            definition,
            resolved_parameters,
            pose,
            visible: true,
            suppressed: false,
            grounded: false,
        }
    }

    #[must_use]
    pub const fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    #[must_use]
    pub const fn suppressed(mut self, suppressed: bool) -> Self {
        self.suppressed = suppressed;
        self
    }

    #[must_use]
    pub const fn grounded(mut self, grounded: bool) -> Self {
        self.grounded = grounded;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), ComponentError> {
        validate_component_label(&self.label)?;
        self.pose.validate()
    }
}

/// Persistent assembly occurrence linked atomically to its creating feature
/// and newly created bodies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComponentInstanceRecord {
    pub id: ComponentInstanceId,
    pub label: String,
    pub definition: ComponentDefinitionRef,
    pub resolved_parameters: EvaluatedParameters,
    pub binding_digest: ParameterBindingDigest,
    pub pose: RigidComponentPose,
    pub visible: bool,
    pub suppressed: bool,
    pub grounded: bool,
    pub created_by: FeatureId,
    pub bodies: Vec<BodyId>,
}

impl ComponentInstanceRecord {
    pub(crate) fn from_draft(
        id: ComponentInstanceId,
        created_by: FeatureId,
        bodies: Vec<BodyId>,
        draft: ComponentInstanceDraft,
    ) -> Result<Self, ComponentError> {
        draft.validate()?;
        if bodies.is_empty() {
            return Err(ComponentError::MissingBodies);
        }
        let binding_digest = draft.resolved_parameters.binding_digest();
        Ok(Self {
            id,
            label: draft.label,
            definition: draft.definition,
            resolved_parameters: draft.resolved_parameters,
            binding_digest,
            pose: draft.pose,
            visible: draft.visible,
            suppressed: draft.suppressed,
            grounded: draft.grounded,
            created_by,
            bodies,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ComponentError> {
        if self.id.get() == 0 {
            return Err(ComponentError::InvalidStableId);
        }
        validate_component_label(&self.label)?;
        self.pose.validate()?;
        if self.bodies.is_empty() {
            return Err(ComponentError::MissingBodies);
        }
        if self.binding_digest != self.resolved_parameters.binding_digest() {
            return Err(ComponentError::BindingDigestMismatch);
        }
        Ok(())
    }
}

/// Structured component-instance rejection.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ComponentError {
    #[error("component definition key is invalid")]
    InvalidDefinitionKey,
    #[error("component definition revision is invalid")]
    InvalidRevision,
    #[error("component content digest must contain exactly 64 hexadecimal characters")]
    InvalidContentDigest,
    #[error("component label is invalid")]
    InvalidLabel,
    #[error("component translation is non-finite or outside the supported model range")]
    InvalidTranslation,
    #[error("component quaternion must be finite and non-zero")]
    InvalidQuaternion,
    #[error("component quaternion must be unit length and sign-canonical")]
    NonCanonicalQuaternion,
    #[error("component occurrence must own at least one body")]
    MissingBodies,
    #[error("component binding digest does not match its resolved assignments")]
    BindingDigestMismatch,
    #[error("stable component IDs must be non-zero")]
    InvalidStableId,
}

fn validate_definition_key(value: &str) -> Result<(), ComponentError> {
    if value.is_empty()
        || value.len() > MAX_DEFINITION_KEY_BYTES
        || value == "."
        || value == ".."
        || value.contains("..")
    {
        return Err(ComponentError::InvalidDefinitionKey);
    }
    let mut characters = value.chars();
    if !characters
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        || !characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(ComponentError::InvalidDefinitionKey);
    }
    Ok(())
}

fn validate_component_label(value: &str) -> Result<(), ComponentError> {
    if value.is_empty()
        || value.len() > crate::MAX_LABEL_BYTES
        || value.chars().any(char::is_control)
    {
        Err(ComponentError::InvalidLabel)
    } else {
        Ok(())
    }
}

fn first_nonzero_is_negative(values: [f64; 4]) -> bool {
    values
        .into_iter()
        .find(|value| *value != 0.0)
        .is_some_and(|value| value.is_sign_negative())
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quaternion_normalization_has_one_deterministic_sign() {
        let positive =
            CanonicalQuaternion::new(0.0, 0.0, 0.0, 2.0).expect("quaternion should normalize");
        let negative = CanonicalQuaternion::new(0.0, 0.0, 0.0, -2.0)
            .expect("equivalent quaternion should normalize");
        assert_eq!(positive, negative);
        assert_eq!(positive.z(), 1.0);
    }

    #[test]
    fn quaternion_serde_rejects_noncanonical_archives() {
        let non_unit = serde_json::json!({"w": 2.0, "x": 0.0, "y": 0.0, "z": 0.0});
        assert!(serde_json::from_value::<CanonicalQuaternion>(non_unit).is_err());
        let negative = serde_json::json!({"w": -1.0, "x": 0.0, "y": 0.0, "z": 0.0});
        assert!(serde_json::from_value::<CanonicalQuaternion>(negative).is_err());
    }

    #[test]
    fn definition_reference_rejects_paths_and_malformed_digests() {
        let revision = ComponentDefinitionRevision::new(1, 2, 3);
        let digest = ComponentContentDigest::from_bytes([7; 32]);
        assert!(ComponentDefinitionRef::new("profiles.2020", revision, digest).is_ok());
        assert!(ComponentDefinitionRef::new("../profile", revision, digest).is_err());
        assert!("ff".parse::<ComponentContentDigest>().is_err());
        assert_eq!(
            "1.2.3"
                .parse::<ComponentDefinitionRevision>()
                .expect("revision should parse"),
            revision
        );
    }

    #[test]
    fn pose_has_no_scale_and_rejects_nonfinite_translation() {
        let translation =
            ComponentTranslation::new(10.0, -20.0, 30.0).expect("translation should validate");
        let pose = RigidComponentPose::new(translation, CanonicalQuaternion::identity());
        assert_eq!(pose.translation.x(), 10.0);
        assert!(ComponentTranslation::new(f64::INFINITY, 0.0, 0.0).is_err());
        assert!(CanonicalQuaternion::new(0.0, 0.0, 0.0, 0.0).is_err());
    }
}

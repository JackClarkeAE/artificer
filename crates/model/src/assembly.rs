//! Persistent assembly hierarchy and rigid-joint intent.
//!
//! Joints reference component occurrences rather than transient bodies.  The
//! document layer owns only the stable graph and validated joint recipe; a
//! later assembly solver is responsible for deriving occurrence poses.

use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

use crate::{ComponentInstanceId, JointId};

/// Maximum number of retained assembly joints in one document.
pub const MAX_JOINTS: usize = 4_096;
/// Maximum UTF-8 byte length of a user-visible joint name.
pub const MAX_JOINT_NAME_BYTES: usize = 128;
/// Maximum absolute joint-origin coordinate in canonical millimetres.
pub const MAX_JOINT_ORIGIN_COORDINATE: f64 = 1.0e12;
/// Maximum absolute revolute limit in canonical radians.
pub const MAX_REVOLUTE_LIMIT_RADIANS: f64 = 1.0e6;

const UNIT_AXIS_TOLERANCE: f64 = 1.0e-12;

/// Parent endpoint of one directed assembly joint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "component", rename_all = "snake_case")]
pub enum JointParent {
    /// Stable assembly origin.
    World,
    /// Another component occurrence in this document.
    Component(ComponentInstanceId),
}

/// Finite joint origin in canonical model millimetres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JointOrigin {
    x: f64,
    y: f64,
    z: f64,
}

impl JointOrigin {
    pub fn new(x: f64, y: f64, z: f64) -> Result<Self, JointError> {
        let origin = Self {
            x: normalize_zero(x),
            y: normalize_zero(y),
            z: normalize_zero(z),
        };
        origin.validate()?;
        Ok(origin)
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

    fn validate(self) -> Result<(), JointError> {
        if [self.x, self.y, self.z]
            .into_iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_JOINT_ORIGIN_COORDINATE)
        {
            Ok(())
        } else {
            Err(JointError::InvalidOrigin)
        }
    }
}

impl<'de> Deserialize<'de> for JointOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            x: f64,
            y: f64,
            z: f64,
        }

        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.x, raw.y, raw.z).map_err(de::Error::custom)
    }
}

/// Deterministic unit direction used by a revolute joint.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JointAxis {
    x: f64,
    y: f64,
    z: f64,
}

impl JointAxis {
    /// Normalizes a finite non-zero vector and canonicalizes signed zero.
    pub fn new(x: f64, y: f64, z: f64) -> Result<Self, JointError> {
        if ![x, y, z].into_iter().all(f64::is_finite) {
            return Err(JointError::InvalidAxis);
        }
        let norm = x.hypot(y).hypot(z);
        if !norm.is_finite() || norm <= f64::EPSILON {
            return Err(JointError::InvalidAxis);
        }
        let axis = Self {
            x: normalize_zero(x / norm),
            y: normalize_zero(y / norm),
            z: normalize_zero(z / norm),
        };
        axis.validate_canonical()?;
        Ok(axis)
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

    fn validate_canonical(self) -> Result<(), JointError> {
        if ![self.x, self.y, self.z].into_iter().all(f64::is_finite) {
            return Err(JointError::InvalidAxis);
        }
        let norm_squared = self
            .x
            .mul_add(self.x, self.y.mul_add(self.y, self.z * self.z));
        if (norm_squared - 1.0).abs() > UNIT_AXIS_TOLERANCE {
            Err(JointError::NonCanonicalAxis)
        } else {
            Ok(())
        }
    }
}

impl<'de> Deserialize<'de> for JointAxis {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            x: f64,
            y: f64,
            z: f64,
        }

        let raw = Raw::deserialize(deserializer)?;
        let axis = Self {
            x: normalize_zero(raw.x),
            y: normalize_zero(raw.y),
            z: normalize_zero(raw.z),
        };
        axis.validate_canonical().map_err(de::Error::custom)?;
        Ok(axis)
    }
}

/// Closed angular travel interval for one revolute joint, in radians.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevoluteLimits {
    min_radians: f64,
    max_radians: f64,
}

impl RevoluteLimits {
    pub fn new(min_radians: f64, max_radians: f64) -> Result<Self, JointError> {
        let limits = Self {
            min_radians: normalize_zero(min_radians),
            max_radians: normalize_zero(max_radians),
        };
        limits.validate()?;
        Ok(limits)
    }

    #[must_use]
    pub const fn min_radians(self) -> f64 {
        self.min_radians
    }

    #[must_use]
    pub const fn max_radians(self) -> f64 {
        self.max_radians
    }

    fn validate(self) -> Result<(), JointError> {
        if ![self.min_radians, self.max_radians]
            .into_iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_REVOLUTE_LIMIT_RADIANS)
            || self.min_radians > self.max_radians
        {
            Err(JointError::InvalidRevoluteLimits)
        } else {
            Ok(())
        }
    }
}

impl<'de> Deserialize<'de> for RevoluteLimits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            min_radians: f64,
            max_radians: f64,
        }

        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.min_radians, raw.max_radians).map_err(de::Error::custom)
    }
}

/// Solver-independent rigid-joint recipe.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JointKind {
    Fixed,
    Revolute {
        origin: JointOrigin,
        axis: JointAxis,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limits: Option<RevoluteLimits>,
    },
}

impl JointKind {
    pub(crate) fn validate(self) -> Result<(), JointError> {
        match self {
            Self::Fixed => Ok(()),
            Self::Revolute {
                origin,
                axis,
                limits,
            } => {
                origin.validate()?;
                axis.validate_canonical()?;
                if let Some(limits) = limits {
                    limits.validate()?;
                }
                Ok(())
            }
        }
    }
}

/// Staged data for a new or replacement joint.
#[derive(Clone, Debug, PartialEq)]
pub struct JointDraft {
    pub name: String,
    pub parent: JointParent,
    pub child: ComponentInstanceId,
    pub kind: JointKind,
    pub enabled: bool,
}

impl JointDraft {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        parent: JointParent,
        child: ComponentInstanceId,
        kind: JointKind,
    ) -> Self {
        Self {
            name: name.into(),
            parent,
            child,
            kind,
            enabled: true,
        }
    }

    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), JointError> {
        validate_joint_name(&self.name)?;
        if self.child.get() == 0 {
            return Err(JointError::InvalidStableId);
        }
        if matches!(self.parent, JointParent::Component(parent) if parent.get() == 0) {
            return Err(JointError::InvalidStableId);
        }
        self.kind.validate()
    }
}

/// Persistent directed edge in the assembly hierarchy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointRecord {
    pub id: JointId,
    pub name: String,
    pub parent: JointParent,
    pub child: ComponentInstanceId,
    pub kind: JointKind,
    pub enabled: bool,
}

impl JointRecord {
    pub(crate) fn from_draft(id: JointId, draft: JointDraft) -> Result<Self, JointError> {
        draft.validate()?;
        Ok(Self {
            id,
            name: draft.name,
            parent: draft.parent,
            child: draft.child,
            kind: draft.kind,
            enabled: draft.enabled,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), JointError> {
        if self.id.get() == 0 {
            return Err(JointError::InvalidStableId);
        }
        JointDraft {
            name: self.name.clone(),
            parent: self.parent,
            child: self.child,
            kind: self.kind,
            enabled: self.enabled,
        }
        .validate()
    }
}

/// Structured validation failure for one joint's local data.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum JointError {
    #[error("joint name must be trimmed, printable, and contain 1 to {MAX_JOINT_NAME_BYTES} bytes")]
    InvalidName,
    #[error("joint origin is non-finite or outside the supported model range")]
    InvalidOrigin,
    #[error("joint axis must be finite and non-zero")]
    InvalidAxis,
    #[error("serialized joint axis must have unit length")]
    NonCanonicalAxis,
    #[error("revolute limits must be finite, ordered, and inside the supported angular range")]
    InvalidRevoluteLimits,
    #[error("stable joint and component IDs must be non-zero")]
    InvalidStableId,
}

pub(crate) fn validate_joint_name(name: &str) -> Result<(), JointError> {
    if name.is_empty()
        || name.len() > MAX_JOINT_NAME_BYTES
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        Err(JointError::InvalidName)
    } else {
        Ok(())
    }
}

const fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

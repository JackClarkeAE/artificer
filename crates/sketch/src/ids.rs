use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

macro_rules! stable_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Constructs an ID when `raw` is non-zero.
            #[must_use]
            pub const fn new(raw: u64) -> Option<Self> {
                match NonZeroU64::new(raw) {
                    Some(raw) => Some(Self(raw)),
                    None => None,
                }
            }

            /// Returns the persisted integer representation.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl TryFrom<u64> for $name {
            type Error = ZeroSketchId;

            fn try_from(raw: u64) -> Result<Self, Self::Error> {
                Self::new(raw).ok_or(ZeroSketchId)
            }
        }

        impl From<$name> for u64 {
            fn from(id: $name) -> Self {
                id.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(formatter)
            }
        }
    };
}

/// A persisted sketch ID was decoded or constructed with the reserved value zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZeroSketchId;

impl fmt::Display for ZeroSketchId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("stable sketch IDs must be non-zero")
    }
}

impl std::error::Error for ZeroSketchId {}

stable_id!(
    SketchPointId,
    "Stable identity of an authored or generated sketch point."
);
stable_id!(
    SketchOperationId,
    "Stable identity of one ordered sketch authoring operation."
);
stable_id!(
    SketchEntityId,
    "Stable identity of one atomic evaluated sketch curve."
);
stable_id!(
    SketchConstraintId,
    "Stable identity of one persisted sketch constraint."
);
stable_id!(
    SketchInputKey,
    "Untyped persisted key underlying a typed sketch input slot."
);

/// Monotonic revision of one editable sketch definition.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SketchRevision(u64);

impl SketchRevision {
    pub const INITIAL: Self = Self(0);

    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Display for SketchRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Typed identity for a model-supplied recipe input.
///
/// The marker prevents a length input from being passed to an angle or integer
/// field. Its wire representation remains a single stable non-zero integer.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SketchInputId<T> {
    key: SketchInputKey,
    marker: PhantomData<fn() -> T>,
}

impl<T> SketchInputId<T> {
    #[must_use]
    pub const fn new(key: SketchInputKey) -> Self {
        Self {
            key,
            marker: PhantomData,
        }
    }

    #[must_use]
    pub const fn key(self) -> SketchInputKey {
        self.key
    }
}

impl<T> Clone for SketchInputId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for SketchInputId<T> {}

impl<T> Serialize for SketchInputId<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.key.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for SketchInputId<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        SketchInputKey::deserialize(deserializer).map(Self::new)
    }
}

/// Non-persistent identity for a live gesture preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DraftId(NonZeroU64);

impl DraftId {
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_ids_reject_zero_and_round_trip_as_numbers() {
        assert_eq!(SketchPointId::new(0), None);
        let id = SketchPointId::new(42).expect("non-zero");
        let json = serde_json::to_string(&id).expect("serialize ID");
        assert_eq!(json, "42");
        assert_eq!(
            serde_json::from_str::<SketchPointId>(&json).expect("deserialize ID"),
            id
        );
        assert!(serde_json::from_str::<SketchPointId>("0").is_err());
    }

    #[test]
    fn typed_input_id_has_the_same_compact_wire_form() {
        let key = SketchInputKey::new(7).expect("non-zero");
        let id = SketchInputId::<u32>::new(key);
        assert_eq!(serde_json::to_string(&id).expect("serialize"), "7");
        let decoded = serde_json::from_str::<SketchInputId<u32>>("7").expect("deserialize");
        assert_eq!(decoded.key(), key);
    }
}

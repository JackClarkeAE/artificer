//! Serializable, implementation-independent contracts for the Artificer kernel.
//!
//! This crate intentionally contains data types only. It has no kernel backend,
//! topology storage, UI, renderer, or foreign geometry dependency.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Hard wire-format ceiling for the first polygon-extrusion capability.
///
/// The kernel repeats this check for typed in-process requests. The custom
/// deserializer below additionally prevents an untrusted JSON array from
/// allocating an arbitrarily large profile before kernel preflight runs.
pub const MAX_EXTRUSION_PROFILE_VERTICES: usize = 256;

/// Kernel-preflight and serialized-request ceilings for planar-region
/// extrusion payloads.
///
/// These limits bound the amount of certification and topology work a single
/// request may ask the native kernel to perform. They are intentionally
/// independent of display tessellation: every entry is an exact profile curve.
pub const MAX_PLANAR_PROFILE_REGIONS: usize = 32;
pub const MAX_PLANAR_PROFILE_LOOPS: usize = 128;
pub const MAX_PLANAR_PROFILE_CURVES: usize = 1_024;

mod bounded_planar_profile {
    use std::fmt;

    use serde::{Deserialize, Deserializer, de};

    use super::{
        MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS,
        PlanarLoop2, PlanarProfile2, PlanarRegion2,
    };

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PlanarProfile2, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct("PlanarProfile2", &["regions"], ProfileVisitor)
    }

    #[derive(Default)]
    struct DecodeBudget {
        regions: usize,
        loops: usize,
        curves: usize,
    }

    struct ProfileVisitor;

    impl<'de> de::Visitor<'de> for ProfileVisitor {
        type Value = PlanarProfile2;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded exact planar profile")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut budget = DecodeBudget::default();
            let mut regions = None;
            while let Some(field) = map.next_key::<String>()? {
                match field.as_str() {
                    "regions" => {
                        if regions.is_some() {
                            return Err(de::Error::duplicate_field("regions"));
                        }
                        regions = Some(map.next_value_seed(RegionsSeed {
                            budget: &mut budget,
                        })?);
                    }
                    _ => {
                        map.next_value::<de::IgnoredAny>()?;
                    }
                }
            }
            Ok(PlanarProfile2 {
                regions: regions.ok_or_else(|| de::Error::missing_field("regions"))?,
            })
        }
    }

    struct RegionsSeed<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::DeserializeSeed<'de> for RegionsSeed<'_> {
        type Value = Vec<PlanarRegion2>;

        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_seq(RegionsVisitor {
                budget: self.budget,
            })
        }
    }

    struct RegionsVisitor<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::Visitor<'de> for RegionsVisitor<'_> {
        type Value = Vec<PlanarRegion2>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_PLANAR_PROFILE_REGIONS} planar material regions"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > MAX_PLANAR_PROFILE_REGIONS)
            {
                return Err(de::Error::custom("planar profile region limit exceeded"));
            }
            let mut regions = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_PLANAR_PROFILE_REGIONS),
            );
            loop {
                if regions.len() == MAX_PLANAR_PROFILE_REGIONS {
                    if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                        return Err(de::Error::custom("planar profile region limit exceeded"));
                    }
                    break;
                }
                let Some(region) = sequence.next_element_seed(RegionSeed {
                    budget: self.budget,
                })?
                else {
                    break;
                };
                self.budget.regions += 1;
                regions.push(region);
            }
            Ok(regions)
        }
    }

    struct RegionSeed<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::DeserializeSeed<'de> for RegionSeed<'_> {
        type Value = PlanarRegion2;

        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_struct(
                "PlanarRegion2",
                &["outer", "holes"],
                RegionVisitor {
                    budget: self.budget,
                },
            )
        }
    }

    struct RegionVisitor<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::Visitor<'de> for RegionVisitor<'_> {
        type Value = PlanarRegion2;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("one bounded planar material region")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut outer = None;
            let mut holes = None;
            while let Some(field) = map.next_key::<String>()? {
                match field.as_str() {
                    "outer" => {
                        if outer.is_some() {
                            return Err(de::Error::duplicate_field("outer"));
                        }
                        outer = Some(map.next_value_seed(LoopSeed {
                            budget: self.budget,
                        })?);
                    }
                    "holes" => {
                        if holes.is_some() {
                            return Err(de::Error::duplicate_field("holes"));
                        }
                        holes = Some(map.next_value_seed(LoopsSeed {
                            budget: self.budget,
                        })?);
                    }
                    _ => {
                        map.next_value::<de::IgnoredAny>()?;
                    }
                }
            }
            Ok(PlanarRegion2 {
                outer: outer.ok_or_else(|| de::Error::missing_field("outer"))?,
                holes: holes.ok_or_else(|| de::Error::missing_field("holes"))?,
            })
        }
    }

    struct LoopsSeed<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::DeserializeSeed<'de> for LoopsSeed<'_> {
        type Value = Vec<PlanarLoop2>;

        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_seq(LoopsVisitor {
                budget: self.budget,
            })
        }
    }

    struct LoopsVisitor<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::Visitor<'de> for LoopsVisitor<'_> {
        type Value = Vec<PlanarLoop2>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "a profile with at most {MAX_PLANAR_PROFILE_LOOPS} loops"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let remaining = MAX_PLANAR_PROFILE_LOOPS.saturating_sub(self.budget.loops);
            if sequence.size_hint().is_some_and(|size| size > remaining) {
                return Err(de::Error::custom("planar profile loop limit exceeded"));
            }
            let mut loops = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(remaining));
            loop {
                if self.budget.loops == MAX_PLANAR_PROFILE_LOOPS {
                    if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                        return Err(de::Error::custom("planar profile loop limit exceeded"));
                    }
                    break;
                }
                let Some(profile_loop) = sequence.next_element_seed(LoopSeed {
                    budget: self.budget,
                })?
                else {
                    break;
                };
                loops.push(profile_loop);
            }
            Ok(loops)
        }
    }

    struct LoopSeed<'a> {
        budget: &'a mut DecodeBudget,
    }

    impl<'de> de::DeserializeSeed<'de> for LoopSeed<'_> {
        type Value = PlanarLoop2;

        fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            if self.budget.loops == MAX_PLANAR_PROFILE_LOOPS {
                return Err(de::Error::custom("planar profile loop limit exceeded"));
            }
            let profile_loop = PlanarLoop2::deserialize(deserializer)?;
            let curves = profile_loop.curves.len();
            let Some(total_curves) = self.budget.curves.checked_add(curves) else {
                return Err(de::Error::custom("planar profile curve limit exceeded"));
            };
            if total_curves > MAX_PLANAR_PROFILE_CURVES {
                return Err(de::Error::custom("planar profile curve limit exceeded"));
            }
            self.budget.loops += 1;
            self.budget.curves = total_curves;
            Ok(profile_loop)
        }
    }
}

mod bounded_planar_curves {
    use std::fmt;

    use serde::{Deserializer, de};

    use super::{MAX_PLANAR_PROFILE_CURVES, PlanarCurve2};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PlanarCurve2>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedCurvesVisitor)
    }

    struct BoundedCurvesVisitor;

    impl<'de> de::Visitor<'de> for BoundedCurvesVisitor {
        type Value = Vec<PlanarCurve2>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_PLANAR_PROFILE_CURVES} exact curves in one profile loop"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > MAX_PLANAR_PROFILE_CURVES)
            {
                return Err(de::Error::custom("planar profile curve limit exceeded"));
            }
            let mut curves = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_PLANAR_PROFILE_CURVES),
            );
            while let Some(curve) = sequence.next_element()? {
                if curves.len() == MAX_PLANAR_PROFILE_CURVES {
                    return Err(de::Error::custom("planar profile curve limit exceeded"));
                }
                curves.push(curve);
            }
            Ok(curves)
        }
    }
}

mod bounded_planar_loops {
    use std::fmt;

    use serde::{Deserializer, de};

    use super::{MAX_PLANAR_PROFILE_LOOPS, PlanarLoop2};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PlanarLoop2>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedLoopsVisitor)
    }

    struct BoundedLoopsVisitor;

    impl<'de> de::Visitor<'de> for BoundedLoopsVisitor {
        type Value = Vec<PlanarLoop2>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_PLANAR_PROFILE_LOOPS} hole loops in one material region"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > MAX_PLANAR_PROFILE_LOOPS)
            {
                return Err(de::Error::custom("planar profile loop limit exceeded"));
            }
            let mut loops = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_PLANAR_PROFILE_LOOPS),
            );
            while let Some(profile_loop) = sequence.next_element()? {
                if loops.len() == MAX_PLANAR_PROFILE_LOOPS {
                    return Err(de::Error::custom("planar profile loop limit exceeded"));
                }
                loops.push(profile_loop);
            }
            Ok(loops)
        }
    }
}

mod bounded_planar_regions {
    use std::fmt;

    use serde::{Deserializer, de};

    use super::{MAX_PLANAR_PROFILE_REGIONS, PlanarRegion2};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PlanarRegion2>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedRegionsVisitor)
    }

    struct BoundedRegionsVisitor;

    impl<'de> de::Visitor<'de> for BoundedRegionsVisitor {
        type Value = Vec<PlanarRegion2>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_PLANAR_PROFILE_REGIONS} planar material regions"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > MAX_PLANAR_PROFILE_REGIONS)
            {
                return Err(de::Error::custom("planar profile region limit exceeded"));
            }
            let mut regions = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_PLANAR_PROFILE_REGIONS),
            );
            while let Some(region) = sequence.next_element()? {
                if regions.len() == MAX_PLANAR_PROFILE_REGIONS {
                    return Err(de::Error::custom("planar profile region limit exceeded"));
                }
                regions.push(region);
            }
            Ok(regions)
        }
    }
}

mod bounded_profile_vertices {
    use std::fmt;

    use serde::{Deserializer, de};

    use super::{MAX_EXTRUSION_PROFILE_VERTICES, Point2};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<Point2>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ProfileVerticesVisitor)
    }

    struct ProfileVerticesVisitor;

    impl<'de> de::Visitor<'de> for ProfileVerticesVisitor {
        type Value = Vec<Point2>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_EXTRUSION_PROFILE_VERTICES} planar profile vertices"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > MAX_EXTRUSION_PROFILE_VERTICES)
            {
                return Err(de::Error::custom(format_args!(
                    "extrusion profile exceeds {MAX_EXTRUSION_PROFILE_VERTICES} vertices"
                )));
            }

            let mut vertices = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_EXTRUSION_PROFILE_VERTICES),
            );
            while let Some(vertex) = sequence.next_element()? {
                if vertices.len() == MAX_EXTRUSION_PROFILE_VERTICES {
                    return Err(de::Error::custom(format_args!(
                        "extrusion profile exceeds {MAX_EXTRUSION_PROFILE_VERTICES} vertices"
                    )));
                }
                vertices.push(vertex);
            }
            Ok(vertices)
        }
    }
}

/// JSON must never silently turn an invalid IEEE-754 value into `null`.
///
/// The native in-process API still accepts typed non-finite values so the
/// kernel can reject them with a structured error. The serialized protocol is
/// deliberately narrower: every floating-point value must be finite.
mod finite_f64 {
    use serde::{Deserialize, Deserializer, Serializer, de, ser};

    pub fn serialize<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if value.is_finite() {
            serializer.serialize_f64(*value)
        } else {
            Err(ser::Error::custom(
                "non-finite floating-point values are not serializable",
            ))
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        if value.is_finite() {
            Ok(value)
        } else {
            Err(de::Error::custom(
                "non-finite floating-point values are not deserializable",
            ))
        }
    }
}

mod finite_option_f64 {
    use serde::{Deserialize, Deserializer, Serializer, de, ser};

    pub fn serialize<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) if value.is_finite() => serializer.serialize_some(value),
            Some(_) => Err(ser::Error::custom(
                "non-finite floating-point values are not serializable",
            )),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<f64>::deserialize(deserializer)?;
        if value.is_none_or(f64::is_finite) {
            Ok(value)
        } else {
            Err(de::Error::custom(
                "non-finite floating-point values are not deserializable",
            ))
        }
    }
}

/// Protocol version understood by this experimental kernel slice.
///
/// Version 1 adds committed whole-snapshot similarity transforms. Version 2
/// adds declarative linear-polygon extrusion in an explicit planar frame.
/// Version 3 adds snapshot-bound linear-profile Add/Cut and exact whole-cap
/// push/pull operations on supported planar B-rep faces. These are capability
/// additions within the experimental v3 command family.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(4);

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u32);

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

macro_rules! fixed_hex_id {
    ($name:ident, $bytes:expr) => {
        #[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $bytes]);

        impl $name {
            pub const ZERO: Self = Self([0; $bytes]);

            #[must_use]
            pub const fn new(bytes: [u8; $bytes]) -> Self {
                Self(bytes)
            }

            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; $bytes] {
                &self.0
            }

            #[must_use]
            pub const fn into_bytes(self) -> [u8; $bytes] {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl From<[u8; $bytes]> for $name {
            fn from(bytes: [u8; $bytes]) -> Self {
                Self(bytes)
            }
        }

        impl From<$name> for [u8; $bytes] {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = ParseFixedHexError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                parse_fixed_hex(value).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }
    };
}

fixed_hex_id!(SnapshotId, 16);
fixed_hex_id!(SemanticDigest, 32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFixedHexError {
    WrongLength { expected: usize, actual: usize },
    InvalidDigit { index: usize },
}

impl fmt::Display for ParseFixedHexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { expected, actual } => {
                write!(
                    formatter,
                    "expected {expected} hexadecimal characters, got {actual}"
                )
            }
            Self::InvalidDigit { index } => {
                write!(formatter, "invalid hexadecimal digit at byte {index}")
            }
        }
    }
}

impl std::error::Error for ParseFixedHexError {}

fn parse_fixed_hex<const N: usize>(value: &str) -> Result<[u8; N], ParseFixedHexError> {
    let expected = N * 2;
    if value.len() != expected {
        return Err(ParseFixedHexError::WrongLength {
            expected,
            actual: value.len(),
        });
    }

    let mut output = [0; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high =
            hex_nibble(pair[0]).ok_or(ParseFixedHexError::InvalidDigit { index: index * 2 })?;
        let low = hex_nibble(pair[1]).ok_or(ParseFixedHexError::InvalidDigit {
            index: index * 2 + 1,
        })?;
        output[index] = (high << 4) | low;
    }
    Ok(output)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct EntityId(pub u64);

impl fmt::Display for EntityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

macro_rules! string_id {
    ($name:ident) => {
        #[derive(
            Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(RequestId);
string_id!(DebugId);
string_id!(DiagnosticCode);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Vertex,
    Edge,
    Coedge,
    Loop,
    Face,
    Shell,
    Solid,
}

impl fmt::Display for EntityKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Vertex => "vertex",
            Self::Edge => "edge",
            Self::Coedge => "coedge",
            Self::Loop => "loop",
            Self::Face => "face",
            Self::Shell => "shell",
            Self::Solid => "solid",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntityRef {
    pub snapshot: SnapshotId,
    pub entity: EntityId,
    pub kind: EntityKind,
}

impl fmt::Display for EntityRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}@{}", self.kind, self.entity, self.snapshot)
    }
}

/// A finite point in a planar parameter space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point2 {
    #[serde(with = "finite_f64")]
    pub x: f64,
    #[serde(with = "finite_f64")]
    pub y: f64,
}

impl Point2 {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    #[must_use]
    pub fn total_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.x
            .total_cmp(&other.x)
            .then_with(|| self.y.total_cmp(&other.y))
    }
}

/// Direction in which a planar circular boundary use travels from its start
/// point to its end point when viewed along the profile frame normal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArcDirection {
    CounterClockwise,
    Clockwise,
}

/// One exact analytic curve use in a closed planar profile loop.
///
/// A circular arc uses the unique non-zero sweep below one full revolution in
/// `direction` from `start` to `end`. A complete circle is represented
/// separately so coincident arc endpoints never carry ambiguous intent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanarCurve2 {
    Line {
        start: Point2,
        end: Point2,
    },
    CircularArc {
        center: Point2,
        start: Point2,
        end: Point2,
        direction: ArcDirection,
    },
    Circle {
        center: Point2,
        #[serde(with = "finite_f64")]
        radius: f64,
        direction: ArcDirection,
    },
    Bspline {
        degree: usize,
        control_points: Vec<Point2>,
        knots: Vec<f64>,
        weights: Option<Vec<f64>>,
    },
}

impl PlanarCurve2 {
    #[must_use]
    pub fn is_finite(&self) -> bool {
        match self {
            Self::Line { start, end } => start.is_finite() && end.is_finite(),
            Self::CircularArc {
                center, start, end, ..
            } => center.is_finite() && start.is_finite() && end.is_finite(),
            Self::Circle { center, radius, .. } => center.is_finite() && radius.is_finite(),
            Self::Bspline {
                control_points,
                knots,
                weights,
                ..
            } => {
                control_points.iter().all(|pt| pt.is_finite())
                    && knots.iter().all(|k| k.is_finite())
                    && weights
                        .as_ref()
                        .is_none_or(|w| w.iter().all(|val| val.is_finite() && *val > 0.0))
            }
        }
    }
}

/// One ordered, connected, closed boundary loop.
///
/// Outer loops are counter-clockwise and hole loops clockwise in the profile
/// frame. The kernel re-certifies ordering, closure, winding, intersections,
/// and minimum feature separation before construction.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanarLoop2 {
    #[serde(deserialize_with = "bounded_planar_curves::deserialize")]
    pub curves: Vec<PlanarCurve2>,
}

impl PlanarLoop2 {
    #[must_use]
    pub fn from_polygon(vertices: &[Point2]) -> Self {
        let curves = (0..vertices.len())
            .map(|index| PlanarCurve2::Line {
                start: vertices[index],
                end: vertices[(index + 1) % vertices.len()],
            })
            .collect();
        Self { curves }
    }
}

/// One connected planar material region: an outer boundary and zero or more
/// non-touching holes strictly nested inside it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanarRegion2 {
    pub outer: PlanarLoop2,
    #[serde(deserialize_with = "bounded_planar_loops::deserialize")]
    pub holes: Vec<PlanarLoop2>,
}

impl PlanarRegion2 {
    #[must_use]
    pub fn from_polygon(vertices: &[Point2]) -> Self {
        Self {
            outer: PlanarLoop2::from_polygon(vertices),
            holes: Vec::new(),
        }
    }
}

/// The axis of a revolve, in the profile's own frame.
///
/// Two points rather than a point and a direction: a sketch centreline is
/// already two points, and a degenerate axis is then one equality check away
/// rather than a normalisation that silently succeeds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanarAxis2 {
    pub start: Point2,
    pub end: Point2,
}

impl PlanarAxis2 {
    #[must_use]
    pub const fn new(start: Point2, end: Point2) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.start.is_finite() && self.end.is_finite()
    }
}

/// How far a revolve sweeps.
///
/// An enum rather than an angle so that partial revolves extend this contract
/// later instead of reinterpreting a number whose full-turn value was special.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RevolveAngle {
    FullTurn,
}

/// A deterministic set of disjoint planar material regions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanarProfile2 {
    #[serde(deserialize_with = "bounded_planar_regions::deserialize")]
    pub regions: Vec<PlanarRegion2>,
}

impl PlanarProfile2 {
    #[must_use]
    pub fn from_polygon(vertices: &[Point2]) -> Self {
        Self {
            regions: vec![PlanarRegion2::from_polygon(vertices)],
        }
    }

    #[must_use]
    pub fn curve_count(&self) -> usize {
        self.regions
            .iter()
            .map(|region| {
                region.outer.curves.len()
                    + region
                        .holes
                        .iter()
                        .map(|profile_loop| profile_loop.curves.len())
                        .sum::<usize>()
            })
            .sum()
    }

    #[must_use]
    pub fn loop_count(&self) -> usize {
        self.regions
            .iter()
            .map(|region| 1 + region.holes.len())
            .sum()
    }
}

impl fmt::Display for Point2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "({:.17e}, {:.17e})", self.x, self.y)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point3 {
    #[serde(with = "finite_f64")]
    pub x: f64,
    #[serde(with = "finite_f64")]
    pub y: f64,
    #[serde(with = "finite_f64")]
    pub z: f64,
}

impl Point3 {
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    #[must_use]
    pub fn total_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.x
            .total_cmp(&other.x)
            .then_with(|| self.y.total_cmp(&other.y))
            .then_with(|| self.z.total_cmp(&other.z))
    }
}

impl fmt::Display for Point3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "({:.17e}, {:.17e}, {:.17e})",
            self.x, self.y, self.z
        )
    }
}

/// A world-space vector. Keeping vectors distinct from points prevents
/// translations from being accidentally interpreted as positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vector3 {
    #[serde(with = "finite_f64")]
    pub x: f64,
    #[serde(with = "finite_f64")]
    pub y: f64,
    #[serde(with = "finite_f64")]
    pub z: f64,
}

impl Vector3 {
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

/// A caller-supplied placement for planar parameter coordinates.
///
/// The kernel treats `u` and `v` as directions, robustly normalizes `u`, and
/// orthogonalizes `v` while preserving the right-handed orientation implied by
/// `u × v`. Zero or near-parallel axes are rejected rather than guessed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanarFrame3 {
    pub origin: Point3,
    pub u: Vector3,
    pub v: Vector3,
}

impl PlanarFrame3 {
    #[must_use]
    pub const fn new(origin: Point3, u: Vector3, v: Vector3) -> Self {
        Self { origin, u, v }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.origin.is_finite() && self.u.is_finite() && self.v.is_finite()
    }
}

/// A rotation quaternion in scalar-first `(w, x, y, z)` order.
///
/// Kernels normalize this value robustly and reject the zero quaternion.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotationQuaternion {
    #[serde(with = "finite_f64")]
    pub w: f64,
    #[serde(with = "finite_f64")]
    pub x: f64,
    #[serde(with = "finite_f64")]
    pub y: f64,
    #[serde(with = "finite_f64")]
    pub z: f64,
}

impl RotationQuaternion {
    pub const IDENTITY: Self = Self::new(1.0, 0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(w: f64, x: f64, y: f64, z: f64) -> Self {
        Self { w, x, y, z }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.w.is_finite() && self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Default for RotationQuaternion {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Orientation-preserving uniform scale and rotation about the world origin,
/// followed by a world-space translation.
///
/// `p' = uniform_scale * rotation(p) + translation`
///
/// Editing pivots belong to the caller's interaction model and are converted
/// to this canonical, non-redundant representation before execution.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimilarityTransform3 {
    pub translation: Vector3,
    pub rotation: RotationQuaternion,
    #[serde(with = "finite_f64")]
    pub uniform_scale: f64,
}

impl SimilarityTransform3 {
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            translation: Vector3::new(0.0, 0.0, 0.0),
            rotation: RotationQuaternion::IDENTITY,
            uniform_scale: 1.0,
        }
    }
}

impl Default for SimilarityTransform3 {
    fn default() -> Self {
        Self::identity()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb3 {
    pub min: Point3,
    pub max: Point3,
}

impl Aabb3 {
    #[must_use]
    pub const fn new(min: Point3, max: Point3) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.min.is_finite() && self.max.is_finite()
    }

    #[must_use]
    pub fn is_ordered(self) -> bool {
        self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z
    }
}

impl fmt::Display for Aabb3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{} .. {}]", self.min, self.max)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LengthUnit {
    Millimetre,
    Metre,
    Inch,
}

impl fmt::Display for LengthUnit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Millimetre => "mm",
            Self::Metre => "m",
            Self::Inch => "in",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrecisionPolicy {
    pub unit: LengthUnit,
    #[serde(with = "finite_f64")]
    pub modeling_resolution: f64,
    #[serde(with = "finite_f64")]
    pub linear_agreement: f64,
    #[serde(with = "finite_f64")]
    pub angular_agreement_radians: f64,
    #[serde(with = "finite_f64")]
    pub parameter_resolution: f64,
    #[serde(with = "finite_f64")]
    pub approximation_budget: f64,
    #[serde(with = "finite_f64")]
    pub max_entity_uncertainty: f64,
    #[serde(with = "finite_f64")]
    pub max_operation_uncertainty: f64,
    pub max_iterations: u32,
    pub max_subdivisions: u32,
    #[serde(with = "finite_f64")]
    pub max_abs_coordinate: f64,
    #[serde(with = "finite_f64")]
    pub min_feature_size: f64,
}

impl Default for PrecisionPolicy {
    fn default() -> Self {
        Self {
            unit: LengthUnit::Millimetre,
            modeling_resolution: 1.0e-6,
            linear_agreement: 1.0e-9,
            angular_agreement_radians: 1.0e-10,
            parameter_resolution: 1.0e-12,
            approximation_budget: 1.0e-5,
            max_entity_uncertainty: 1.0e-6,
            max_operation_uncertainty: 1.0e-5,
            max_iterations: 128,
            max_subdivisions: 64,
            max_abs_coordinate: 1.0e9,
            min_feature_size: 1.0e-5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub protocol_version: ProtocolVersion,
    pub request_id: RequestId,
    pub expected_snapshot: SnapshotId,
    pub precision: PrecisionPolicy,
    pub command: KernelCommand,
}

/// Two-snapshot request for a regularized native Boolean. Both immutable
/// operands are named explicitly so stale tools cannot be combined silently.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BooleanRequest {
    pub protocol_version: ProtocolVersion,
    pub request_id: RequestId,
    pub expected_target_snapshot: SnapshotId,
    pub expected_tool_snapshot: SnapshotId,
    pub precision: PrecisionPolicy,
    pub operation: BooleanOperation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BooleanOperation {
    Union,
    Difference,
    Intersection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KernelCommand {
    MakeCuboid {
        origin: Point3,
        #[serde(with = "finite_f64")]
        size_x: f64,
        #[serde(with = "finite_f64")]
        size_y: f64,
        #[serde(with = "finite_f64")]
        size_z: f64,
    },
    /// Full-turn revolve of one axis-aligned rectangular radial section. The
    /// exact result is a cylinder or annular cylinder with planar caps.
    MakeRevolvedAnnulus {
        frame: PlanarFrame3,
        #[serde(with = "finite_f64")]
        inner_radius: f64,
        #[serde(with = "finite_f64")]
        outer_radius: f64,
        #[serde(with = "finite_f64")]
        height: f64,
    },
    /// Applies one orientation-preserving similarity transform to every
    /// entity in the current immutable snapshot.
    ///
    /// This first committed-transform capability deliberately targets the
    /// snapshot as a whole. Body-local transforms can be added later without
    /// putting snapshot-local allocator IDs into declarative case fixtures.
    TransformSnapshot { transform: SimilarityTransform3 },
    /// Constructs a watertight prism by extruding one certified simple linear
    /// planar polygon along the frame's positive normal.
    ExtrudePolygon {
        frame: PlanarFrame3,
        #[serde(deserialize_with = "bounded_profile_vertices::deserialize")]
        vertices: Vec<Point2>,
        #[serde(with = "finite_f64")]
        distance: f64,
    },
    /// Constructs exact prismatic material from one or more certified planar
    /// regions. Each region may contain nested profile holes. Linear and
    /// analytic curve uses remain explicit on the wire and are never silently
    /// converted to display polygons.
    ExtrudePlanarProfile {
        frame: PlanarFrame3,
        #[serde(deserialize_with = "bounded_planar_profile::deserialize")]
        profile: PlanarProfile2,
        #[serde(with = "finite_f64")]
        distance: f64,
    },
    /// Revolves one certified planar region a full turn about an axis lying in
    /// its own frame.
    ///
    /// The profile must sit entirely on one side of the axis, touching it only
    /// along axis-collinear segments or at the ends of arcs — a line meeting
    /// the axis obliquely would sweep a cone apex, which is a singularity
    /// rather than a pole, and is refused.
    RevolvePlanarProfile {
        frame: PlanarFrame3,
        #[serde(deserialize_with = "bounded_planar_profile::deserialize")]
        profile: PlanarProfile2,
        axis: PlanarAxis2,
        angle: RevolveAngle,
    },
    /// Adds an outward linear-profile boss or removes a blind/through pocket
    /// from one supported axis-aligned planar boundary patch.
    ///
    /// This deliberately narrow topology-editing command is distinct from the
    /// empty-snapshot `ExtrudePolygon` constructor. The target is bound to the
    /// expected immutable snapshot, and the kernel revalidates the face,
    /// support frame, profile containment, operation, and distance before
    /// publishing a replacement solid.
    ExtrudeFaceProfile {
        target_face: EntityRef,
        frame: PlanarFrame3,
        #[serde(deserialize_with = "bounded_profile_vertices::deserialize")]
        vertices: Vec<Point2>,
        #[serde(with = "finite_f64")]
        distance: f64,
        operation: FaceExtrusionOperation,
    },
    /// Adds or removes one exact planar profile, including profile holes, on
    /// a snapshot-bound planar B-rep face.
    ExtrudeFacePlanarProfile {
        target_face: EntityRef,
        frame: PlanarFrame3,
        #[serde(deserialize_with = "bounded_planar_profile::deserialize")]
        profile: PlanarProfile2,
        #[serde(with = "finite_f64")]
        distance: f64,
        operation: FaceExtrusionOperation,
    },
    /// Moves one certified, unholed extrusion-cap face along its authoritative
    /// outward normal without changing topology cardinality.
    ///
    /// Positive distance extends the solid (Add); negative distance shortens
    /// it (Cut). The selected B-rep face is the exact profile, so callers do
    /// not duplicate its boundary through a sketch or tessellation payload.
    PushPullFace {
        target_face: EntityRef,
        #[serde(with = "finite_f64")]
        distance: f64,
    },
    /// Exact cylindrical blind/through hole on a supported planar face.
    DrillHole {
        target_face: EntityRef,
        frame: PlanarFrame3,
        center: Point2,
        #[serde(with = "finite_f64")]
        diameter: f64,
        #[serde(with = "finite_f64")]
        depth: f64,
    },
    /// Adds one straight rectangular rib around a face-local centre line.
    AddRib {
        target_face: EntityRef,
        frame: PlanarFrame3,
        start: Point2,
        end: Point2,
        #[serde(with = "finite_f64")]
        thickness: f64,
        #[serde(with = "finite_f64")]
        height: f64,
    },
    /// Mirrors a planar B-rep across one world-space plane.
    MirrorSnapshot {
        plane_origin: Point3,
        plane_normal: Vector3,
    },
    /// Creates a multi-solid linear pattern of the complete planar snapshot.
    LinearPatternSnapshot {
        direction: Vector3,
        #[serde(with = "finite_f64")]
        spacing: f64,
        count: u16,
    },
    /// Finishes one complete axis-aligned cuboid edge in the first exact,
    /// production-gated domain.
    FinishEdge {
        target_edge: EntityRef,
        kind: EdgeFinishKind,
        #[serde(with = "finite_f64")]
        distance: f64,
    },
    /// Finishes a compatible set of complete edges as one atomic feature.
    /// The first exact domain accepts one to four parallel edges of the same
    /// axis-aligned cuboid so all affected corners are reconstructed together.
    FinishEdges {
        target_edges: Vec<EntityRef>,
        kind: EdgeFinishKind,
        #[serde(with = "finite_f64")]
        distance: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeFinishKind {
    Chamfer,
    Fillet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaceExtrusionOperation {
    Add,
    Cut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelStage {
    Protocol,
    Preflight,
    Construction,
    Validation,
    Commit,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiagnosticSubject {
    Entity { entity: EntityRef },
    Debug { debug_id: DebugId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantityKind {
    Length,
    Angle,
    Parameter,
    Curvature,
    Unitless,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NumericInterval {
    #[serde(with = "finite_option_f64")]
    pub min: Option<f64>,
    #[serde(with = "finite_option_f64")]
    pub max: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticMeasurement {
    pub quantity: QuantityKind,
    #[serde(with = "finite_f64")]
    pub measured: f64,
    pub allowed: NumericInterval,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub stage: KernelStage,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<DiagnosticSubject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<DiagnosticMeasurement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl Diagnostic {
    #[must_use]
    pub fn ordering_key(
        &self,
    ) -> (
        &DiagnosticCode,
        KernelStage,
        &[DiagnosticSubject],
        &[String],
    ) {
        (&self.code, self.stage, &self.subjects, &self.path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelErrorCode {
    StaleSnapshot,
    PrecisionPolicyMismatch,
    InvalidInput,
    ValidationFailed,
    Cancelled,
    ResourceLimitExceeded,
    Unsupported,
    NumericallyIndeterminate,
    InternalFailure,
}

impl fmt::Display for KernelErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let serialized = match self {
            Self::StaleSnapshot => "stale_snapshot",
            Self::PrecisionPolicyMismatch => "precision_policy_mismatch",
            Self::InvalidInput => "invalid_input",
            Self::ValidationFailed => "validation_failed",
            Self::Cancelled => "cancelled",
            Self::ResourceLimitExceeded => "resource_limit_exceeded",
            Self::Unsupported => "unsupported",
            Self::NumericallyIndeterminate => "numerically_indeterminate",
            Self::InternalFailure => "internal_failure",
        };
        formatter.write_str(serialized)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KernelError {
    pub code: KernelErrorCode,
    pub stage: KernelStage,
    pub input_snapshot: SnapshotId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl fmt::Display for KernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {:?}: {}",
            self.code, self.stage, self.message
        )
    }
}

impl std::error::Error for KernelError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRelation {
    Generated,
    Modified,
    Deleted,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OperationRole {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
}

impl OperationRole {
    #[must_use]
    pub fn new(name: impl Into<String>, ordinal: Option<u32>) -> Self {
        Self {
            name: name.into(),
            ordinal,
        }
    }
}

impl fmt::Display for OperationRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ordinal {
            Some(ordinal) => write!(formatter, "{}[{ordinal}]", self.name),
            None => formatter.write_str(&self.name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub relation: HistoryRelation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<EntityRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<EntityRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<OperationRole>,
}

impl HistoryRecord {
    pub fn sort_entities(&mut self) {
        self.inputs.sort_unstable();
        self.outputs.sort_unstable();
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TopologyCounts {
    pub vertices: u64,
    pub edges: u64,
    pub coedges: u64,
    pub loops: u64,
    pub faces: u64,
    pub shells: u64,
    pub solids: u64,
}

impl TopologyCounts {
    #[must_use]
    pub const fn total(self) -> u64 {
        self.vertices
            + self.edges
            + self.coedges
            + self.loops
            + self.faces
            + self.shells
            + self.solids
    }

    #[must_use]
    pub const fn get(self, kind: EntityKind) -> u64 {
        match kind {
            EntityKind::Vertex => self.vertices,
            EntityKind::Edge => self.edges,
            EntityKind::Coedge => self.coedges,
            EntityKind::Loop => self.loops,
            EntityKind::Face => self.faces,
            EntityKind::Shell => self.shells,
            EntityKind::Solid => self.solids,
        }
    }
}

impl fmt::Display for TopologyCounts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "V={} E={} C={} L={} F={} Sh={} S={}",
            self.vertices,
            self.edges,
            self.coedges,
            self.loops,
            self.faces,
            self.shells,
            self.solids
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationProfile {
    Topology,
    ClosedShell,
    Solid,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub profile: ValidationProfile,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    pub fn sort_diagnostics(&mut self) {
        self.diagnostics
            .sort_by(|left, right| left.ordering_key().cmp(&right.ordering_key()));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationReport {
    pub input_snapshot: SnapshotId,
    pub output_snapshot: SnapshotId,
    pub semantic_digest: SemanticDigest,
    pub topology: TopologyCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Aabb3>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryRecord>,
    pub validation: ValidationReport,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Diagnostic>,
}

impl OperationReport {
    pub fn sort_deterministically(&mut self) {
        for record in &mut self.history {
            record.sort_entities();
        }
        self.history.sort_unstable();
        self.validation.sort_diagnostics();
        self.warnings
            .sort_by(|left, right| left.ordering_key().cmp(&right.ordering_key()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(byte: u8) -> SnapshotId {
        SnapshotId::new([byte; 16])
    }

    fn entity(snapshot: SnapshotId, id: u64, kind: EntityKind) -> EntityRef {
        EntityRef {
            snapshot,
            entity: EntityId(id),
            kind,
        }
    }

    #[test]
    fn fixed_width_ids_use_canonical_lowercase_hex_json() {
        let id = SnapshotId::new([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0xfe, 0xff,
        ]);

        assert_eq!(id.to_string(), "000102030405060708090a0b0c0dfeff");
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"000102030405060708090a0b0c0dfeff\""
        );

        let uppercase: SnapshotId = "000102030405060708090A0B0C0DFEFF".parse().unwrap();
        assert_eq!(uppercase, id);
        assert_eq!(uppercase.to_string(), id.to_string());
    }

    #[test]
    fn fixed_width_ids_reject_wrong_length_and_non_hex_text() {
        assert_eq!(
            "00".parse::<SnapshotId>(),
            Err(ParseFixedHexError::WrongLength {
                expected: 32,
                actual: 2
            })
        );
        assert_eq!(
            "000102030405060708090a0b0c0dfefz".parse::<SnapshotId>(),
            Err(ParseFixedHexError::InvalidDigit { index: 31 })
        );
        assert!(serde_json::from_str::<SemanticDigest>("\"not-a-digest\"").is_err());
    }

    #[test]
    fn cuboid_request_has_a_stable_tagged_json_shape() {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/cuboid/0"),
            expected_snapshot: snapshot(0),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::new(1.0, 2.0, 3.0),
                size_x: 4.0,
                size_y: 5.0,
                size_z: 6.0,
            },
        };

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["protocol_version"], json!(4));
        assert_eq!(encoded["request_id"], json!("case/cuboid/0"));
        assert_eq!(
            encoded["expected_snapshot"],
            json!("00000000000000000000000000000000")
        );
        assert_eq!(
            encoded["command"],
            json!({
                "type": "make_cuboid",
                "origin": { "x": 1.0, "y": 2.0, "z": 3.0 },
                "size_x": 4.0,
                "size_y": 5.0,
                "size_z": 6.0
            })
        );

        let decoded: ExecuteRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn similarity_request_has_a_stable_tagged_json_shape() {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/transform/0"),
            expected_snapshot: snapshot(3),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::TransformSnapshot {
                transform: SimilarityTransform3 {
                    translation: Vector3::new(10.0, -5.0, 2.0),
                    rotation: RotationQuaternion::new(
                        std::f64::consts::FRAC_1_SQRT_2,
                        0.0,
                        0.0,
                        std::f64::consts::FRAC_1_SQRT_2,
                    ),
                    uniform_scale: 2.0,
                },
            },
        };

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded["command"],
            json!({
                "type": "transform_snapshot",
                "transform": {
                    "translation": { "x": 10.0, "y": -5.0, "z": 2.0 },
                    "rotation": {
                        "w": std::f64::consts::FRAC_1_SQRT_2,
                        "x": 0.0,
                        "y": 0.0,
                        "z": std::f64::consts::FRAC_1_SQRT_2
                    },
                    "uniform_scale": 2.0
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<ExecuteRequest>(encoded).unwrap(),
            request
        );
    }

    #[test]
    fn extrusion_request_has_a_stable_finite_json_shape() {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/extrude/0"),
            expected_snapshot: SnapshotId::ZERO,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudePolygon {
                frame: PlanarFrame3::new(
                    Point3::new(1.0, 2.0, 3.0),
                    Vector3::new(2.0, 0.0, 0.0),
                    Vector3::new(0.0, 3.0, 0.0),
                ),
                vertices: vec![
                    Point2::new(0.0, 0.0),
                    Point2::new(2.0, 0.0),
                    Point2::new(0.0, 3.0),
                ],
                distance: 4.0,
            },
        };

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded["command"],
            json!({
                "type": "extrude_polygon",
                "frame": {
                    "origin": { "x": 1.0, "y": 2.0, "z": 3.0 },
                    "u": { "x": 2.0, "y": 0.0, "z": 0.0 },
                    "v": { "x": 0.0, "y": 3.0, "z": 0.0 }
                },
                "vertices": [
                    { "x": 0.0, "y": 0.0 },
                    { "x": 2.0, "y": 0.0 },
                    { "x": 0.0, "y": 3.0 }
                ],
                "distance": 4.0
            })
        );
        assert_eq!(
            serde_json::from_value::<ExecuteRequest>(encoded).unwrap(),
            request
        );

        let mut invalid = request;
        let KernelCommand::ExtrudePolygon { vertices, .. } = &mut invalid.command else {
            unreachable!("extrusion request constructed above")
        };
        vertices[0].x = f64::NAN;
        assert!(serde_json::to_string(&invalid).is_err());
    }

    #[test]
    fn planar_region_request_round_trips_lines_arcs_circles_and_holes() {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/planar-profile/0"),
            expected_snapshot: SnapshotId::ZERO,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudePlanarProfile {
                frame: PlanarFrame3::new(
                    Point3::default(),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                profile: PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: PlanarLoop2 {
                            curves: vec![
                                PlanarCurve2::Line {
                                    start: Point2::new(-2.0, 0.0),
                                    end: Point2::new(2.0, 0.0),
                                },
                                PlanarCurve2::CircularArc {
                                    center: Point2::new(0.0, 0.0),
                                    start: Point2::new(2.0, 0.0),
                                    end: Point2::new(-2.0, 0.0),
                                    direction: ArcDirection::CounterClockwise,
                                },
                            ],
                        },
                        holes: vec![PlanarLoop2 {
                            curves: vec![PlanarCurve2::Circle {
                                center: Point2::new(0.0, 0.5),
                                radius: 0.25,
                                direction: ArcDirection::Clockwise,
                            }],
                        }],
                    }],
                },
                distance: 3.0,
            },
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["command"]["type"], json!("extrude_planar_profile"));
        assert_eq!(
            encoded["command"]["profile"]["regions"][0]["outer"]["curves"][1]["type"],
            json!("circular_arc")
        );
        assert_eq!(
            encoded["command"]["profile"]["regions"][0]["holes"][0]["curves"][0]["type"],
            json!("circle")
        );
        assert_eq!(
            serde_json::from_value::<ExecuteRequest>(encoded).unwrap(),
            request
        );
    }

    #[test]
    fn serialized_planar_profiles_reject_every_resource_limit() {
        fn request(profile: PlanarProfile2) -> ExecuteRequest {
            ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::from("case/planar-profile/limit"),
                expected_snapshot: SnapshotId::ZERO,
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        Point3::default(),
                        Vector3::new(1.0, 0.0, 0.0),
                        Vector3::new(0.0, 1.0, 0.0),
                    ),
                    profile,
                    distance: 1.0,
                },
            }
        }
        fn line_loop() -> PlanarLoop2 {
            PlanarLoop2 {
                curves: vec![PlanarCurve2::Line {
                    start: Point2::default(),
                    end: Point2::new(1.0, 0.0),
                }],
            }
        }
        fn rejects(request: ExecuteRequest) {
            let json = serde_json::to_string(&request).unwrap();
            assert!(serde_json::from_str::<ExecuteRequest>(&json).is_err());
        }

        rejects(request(PlanarProfile2 {
            regions: (0..=MAX_PLANAR_PROFILE_REGIONS)
                .map(|_| PlanarRegion2 {
                    outer: line_loop(),
                    holes: Vec::new(),
                })
                .collect(),
        }));
        rejects(request(PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: line_loop(),
                holes: (0..MAX_PLANAR_PROFILE_LOOPS).map(|_| line_loop()).collect(),
            }],
        }));
        rejects(request(PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: (0..=MAX_PLANAR_PROFILE_CURVES)
                        .map(|_| PlanarCurve2::Line {
                            start: Point2::default(),
                            end: Point2::new(1.0, 0.0),
                        })
                        .collect(),
                },
                holes: Vec::new(),
            }],
        }));
        let curves = |count: usize| PlanarLoop2 {
            curves: (0..count)
                .map(|index| PlanarCurve2::Line {
                    start: Point2::new(index as f64, 0.0),
                    end: Point2::new(index as f64 + 1.0, 0.0),
                })
                .collect(),
        };
        rejects(request(PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: curves(MAX_PLANAR_PROFILE_CURVES / 2 + 1),
                holes: vec![curves(MAX_PLANAR_PROFILE_CURVES / 2)],
            }],
        }));
    }

    #[test]
    fn face_extrusion_request_has_a_stable_snapshot_bound_json_shape() {
        let owner = snapshot(3);
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/face-extrude/cut"),
            expected_snapshot: owner,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudeFaceProfile {
                target_face: entity(owner, 17, EntityKind::Face),
                frame: PlanarFrame3::new(
                    Point3::new(5.0, 4.0, 6.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                vertices: vec![
                    Point2::new(-2.0, -1.0),
                    Point2::new(2.0, -1.0),
                    Point2::new(2.0, 1.0),
                    Point2::new(-2.0, 1.0),
                ],
                distance: 3.0,
                operation: FaceExtrusionOperation::Cut,
            },
        };

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded["command"],
            json!({
                "type": "extrude_face_profile",
                "target_face": {
                    "snapshot": "03030303030303030303030303030303",
                    "entity": 17,
                    "kind": "face"
                },
                "frame": {
                    "origin": { "x": 5.0, "y": 4.0, "z": 6.0 },
                    "u": { "x": 1.0, "y": 0.0, "z": 0.0 },
                    "v": { "x": 0.0, "y": 1.0, "z": 0.0 }
                },
                "vertices": [
                    { "x": -2.0, "y": -1.0 },
                    { "x": 2.0, "y": -1.0 },
                    { "x": 2.0, "y": 1.0 },
                    { "x": -2.0, "y": 1.0 }
                ],
                "distance": 3.0,
                "operation": "cut"
            })
        );
        assert_eq!(
            serde_json::from_value::<ExecuteRequest>(encoded).unwrap(),
            request
        );
    }

    #[test]
    fn face_push_pull_request_has_a_stable_signed_json_shape() {
        let owner = snapshot(7);
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/face-push-pull/cut"),
            expected_snapshot: owner,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::PushPullFace {
                target_face: entity(owner, 41, EntityKind::Face),
                distance: -2.5,
            },
        };

        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded["command"],
            json!({
                "type": "push_pull_face",
                "target_face": {
                    "snapshot": "07070707070707070707070707070707",
                    "entity": 41,
                    "kind": "face"
                },
                "distance": -2.5
            })
        );
        assert_eq!(
            serde_json::from_value::<ExecuteRequest>(encoded).unwrap(),
            request
        );

        let mut invalid = request;
        let KernelCommand::PushPullFace { distance, .. } = &mut invalid.command else {
            unreachable!("push/pull request constructed above")
        };
        *distance = f64::INFINITY;
        assert!(serde_json::to_string(&invalid).is_err());
    }

    #[test]
    fn extrusion_profile_json_is_bounded_before_kernel_preflight() {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/extrude/oversized-wire-profile"),
            expected_snapshot: SnapshotId::ZERO,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudePolygon {
                frame: PlanarFrame3::new(
                    Point3::default(),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                vertices: vec![Point2::new(0.0, 0.0); MAX_EXTRUSION_PROFILE_VERTICES + 1],
                distance: 4.0,
            },
        };

        let encoded = serde_json::to_string(&request).unwrap();
        let error = serde_json::from_str::<ExecuteRequest>(&encoded).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("extrusion profile exceeds 256 vertices")
        );
    }

    #[test]
    fn non_finite_protocol_values_fail_serialization_instead_of_becoming_null() {
        let transform_request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/transform/non-finite"),
            expected_snapshot: snapshot(0),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::TransformSnapshot {
                transform: SimilarityTransform3 {
                    translation: Vector3::new(f64::NAN, 0.0, 0.0),
                    ..SimilarityTransform3::identity()
                },
            },
        };
        assert!(serde_json::to_string(&transform_request).is_err());

        let cuboid_request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/cuboid/non-finite"),
            expected_snapshot: snapshot(0),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::default(),
                size_x: f64::INFINITY,
                size_y: 2.0,
                size_z: 3.0,
            },
        };
        assert!(serde_json::to_string(&cuboid_request).is_err());

        let mut invalid_precision = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::from("case/precision/non-finite"),
            expected_snapshot: snapshot(0),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::TransformSnapshot {
                transform: SimilarityTransform3::identity(),
            },
        };
        invalid_precision.precision.linear_agreement = f64::NEG_INFINITY;
        assert!(serde_json::to_string(&invalid_precision).is_err());
    }

    #[test]
    fn entity_kinds_have_stable_order_and_names() {
        let kinds = [
            EntityKind::Vertex,
            EntityKind::Edge,
            EntityKind::Coedge,
            EntityKind::Loop,
            EntityKind::Face,
            EntityKind::Shell,
            EntityKind::Solid,
        ];

        assert!(kinds.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            kinds.map(|kind| kind.to_string()),
            ["vertex", "edge", "coedge", "loop", "face", "shell", "solid",]
        );
        assert_eq!(
            serde_json::to_value(kinds).unwrap(),
            json!(["vertex", "edge", "coedge", "loop", "face", "shell", "solid"])
        );
    }

    #[test]
    fn point_total_order_is_explicit_and_does_not_claim_geometric_equality() {
        let negative_zero = Point3::new(-0.0, 0.0, 0.0);
        let positive_zero = Point3::new(0.0, 0.0, 0.0);
        let nan = Point3::new(f64::NAN, 0.0, 0.0);

        assert_eq!(negative_zero, positive_zero);
        assert_eq!(
            negative_zero.total_cmp(&positive_zero),
            std::cmp::Ordering::Less
        );
        assert_ne!(nan.total_cmp(&positive_zero), std::cmp::Ordering::Equal);
        assert!(!nan.is_finite());
    }

    #[test]
    fn topology_counts_cover_every_public_entity_kind() {
        let counts = TopologyCounts {
            vertices: 8,
            edges: 12,
            coedges: 24,
            loops: 6,
            faces: 6,
            shells: 1,
            solids: 1,
        };

        assert_eq!(counts.total(), 58);
        assert_eq!(counts.get(EntityKind::Vertex), 8);
        assert_eq!(counts.get(EntityKind::Edge), 12);
        assert_eq!(counts.get(EntityKind::Coedge), 24);
        assert_eq!(counts.get(EntityKind::Loop), 6);
        assert_eq!(counts.get(EntityKind::Face), 6);
        assert_eq!(counts.get(EntityKind::Shell), 1);
        assert_eq!(counts.get(EntityKind::Solid), 1);
        assert_eq!(counts.to_string(), "V=8 E=12 C=24 L=6 F=6 Sh=1 S=1");
    }

    #[test]
    fn report_sorting_is_deterministic() {
        let old = snapshot(1);
        let new = snapshot(2);
        let edge_one = entity(new, 1, EntityKind::Edge);
        let edge_two = entity(new, 2, EntityKind::Edge);

        let mut report = OperationReport {
            input_snapshot: old,
            output_snapshot: new,
            semantic_digest: SemanticDigest::new([7; 32]),
            topology: TopologyCounts {
                edges: 2,
                ..TopologyCounts::default()
            },
            bounds: Some(Aabb3::new(Point3::default(), Point3::new(1.0, 1.0, 1.0))),
            history: vec![
                HistoryRecord {
                    relation: HistoryRelation::Generated,
                    inputs: Vec::new(),
                    outputs: vec![edge_two],
                    role: Some(OperationRole::new("cuboid.edge", Some(1))),
                },
                HistoryRecord {
                    relation: HistoryRelation::Generated,
                    inputs: Vec::new(),
                    outputs: vec![edge_one],
                    role: Some(OperationRole::new("cuboid.edge", Some(0))),
                },
            ],
            validation: ValidationReport {
                profile: ValidationProfile::Solid,
                valid: true,
                diagnostics: vec![
                    Diagnostic {
                        code: DiagnosticCode::from("topology.z"),
                        severity: DiagnosticSeverity::Warning,
                        stage: KernelStage::Validation,
                        message: "later".to_owned(),
                        subjects: Vec::new(),
                        path: Vec::new(),
                        measurement: None,
                        details: BTreeMap::new(),
                    },
                    Diagnostic {
                        code: DiagnosticCode::from("topology.a"),
                        severity: DiagnosticSeverity::Warning,
                        stage: KernelStage::Validation,
                        message: "earlier".to_owned(),
                        subjects: Vec::new(),
                        path: Vec::new(),
                        measurement: None,
                        details: BTreeMap::new(),
                    },
                ],
            },
            warnings: Vec::new(),
        };

        report.sort_deterministically();

        assert_eq!(report.history[0].outputs, vec![edge_one]);
        assert_eq!(report.history[1].outputs, vec![edge_two]);
        assert_eq!(report.validation.diagnostics[0].code.as_str(), "topology.a");
        assert_eq!(report.validation.diagnostics[1].code.as_str(), "topology.z");
        assert_eq!(
            serde_json::from_str::<OperationReport>(&serde_json::to_string(&report).unwrap())
                .unwrap(),
            report
        );
    }

    #[test]
    fn kernel_error_codes_are_machine_stable_and_human_readable() {
        let error = KernelError {
            code: KernelErrorCode::StaleSnapshot,
            stage: KernelStage::Protocol,
            input_snapshot: snapshot(4),
            message: "the request targeted an older snapshot".to_owned(),
            diagnostics: Vec::new(),
            details: BTreeMap::from([
                ("actual".to_owned(), snapshot(4).to_string()),
                ("expected".to_owned(), snapshot(3).to_string()),
            ]),
        };

        assert_eq!(error.code.to_string(), "stale_snapshot");
        let encoded = serde_json::to_value(&error).unwrap();
        assert_eq!(encoded["code"], json!("stale_snapshot"));
        assert_eq!(encoded["stage"], json!("protocol"));
        assert!(encoded.get("diagnostics").is_none());
        assert_eq!(
            serde_json::from_value::<KernelError>(encoded).unwrap(),
            error
        );
    }

    #[test]
    fn bspline_planar_curve_serializes_and_validates_finiteness() {
        let bspline = PlanarCurve2::Bspline {
            degree: 2,
            control_points: vec![Point2::new(0.0, 0.0), Point2::new(1.0, 2.0), Point2::new(2.0, 0.0)],
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            weights: Some(vec![1.0, 2.0_f64.sqrt() / 2.0, 1.0]),
        };
        assert!(bspline.is_finite());
        let encoded = serde_json::to_value(&bspline).unwrap();
        assert_eq!(encoded["type"], json!("bspline"));
        assert_eq!(serde_json::from_value::<PlanarCurve2>(encoded).unwrap(), bspline);
    }
}

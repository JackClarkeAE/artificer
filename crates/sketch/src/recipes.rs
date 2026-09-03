use std::collections::BTreeMap;
use std::fmt;

use artificer_protocol::PlanarProfile2;
use serde::{Deserialize, Serialize};

use crate::{
    CurveDirection, SketchEntityId, SketchInputId, SketchInputKey, SketchPoint2, SketchPointId,
};

/// A finite, strictly positive length used by an authoring recipe.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Length(f64);

impl Length {
    pub fn new(value: f64) -> Result<Self, InvalidTypedValue> {
        if value.is_finite() && value > 0.0 {
            Ok(Self(value))
        } else {
            Err(InvalidTypedValue::PositiveFiniteLength)
        }
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

/// A finite directional length. Zero is structurally valid and individual
/// recipes decide whether it would make their output degenerate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SignedLength(f64);

impl SignedLength {
    pub fn new(value: f64) -> Result<Self, InvalidTypedValue> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(InvalidTypedValue::FiniteSignedLength)
        }
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

/// A finite angle in radians. Recipes normalize it only when evaluation needs
/// a canonical direction; the persisted value retains the user's intent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Angle(f64);

impl Angle {
    pub fn radians(value: f64) -> Result<Self, InvalidTypedValue> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(InvalidTypedValue::FiniteAngle)
        }
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

/// A bounded integer recipe value. Tool-specific ranges are checked during
/// evaluation so this type can also represent line and pattern counts later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Integer(u16);

impl Integer {
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidTypedValue {
    PositiveFiniteLength,
    FiniteSignedLength,
    FiniteAngle,
}

impl fmt::Display for InvalidTypedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PositiveFiniteLength => "length must be finite and greater than zero",
            Self::FiniteSignedLength => "signed length must be finite",
            Self::FiniteAngle => "angle must be finite",
        })
    }
}

impl std::error::Error for InvalidTypedValue {}

/// A typed scalar is either a literal or a reference to a model-supplied input.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum SketchValue<T> {
    Literal(T),
    Input(SketchInputId<T>),
}

impl<T: Copy + SketchValueKind> SketchValue<T> {
    pub fn resolve(self, inputs: &SketchInputValues) -> Result<T, UnresolvedSketchInput> {
        match self {
            Self::Literal(value) => Ok(value),
            Self::Input(id) => T::resolve(inputs, id.key()).ok_or(UnresolvedSketchInput {
                key: id.key(),
                expected: T::KIND,
            }),
        }
    }
}

pub trait SketchValueKind: Sized {
    const KIND: SketchInputKind;

    fn resolve(inputs: &SketchInputValues, key: SketchInputKey) -> Option<Self>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SketchInputKind {
    Length,
    SignedLength,
    Angle,
    Integer,
}

/// Resolved, dimensionally typed recipe values supplied by the model layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SketchInputValues {
    lengths: BTreeMap<SketchInputKey, Length>,
    signed_lengths: BTreeMap<SketchInputKey, SignedLength>,
    angles: BTreeMap<SketchInputKey, Angle>,
    integers: BTreeMap<SketchInputKey, Integer>,
}

impl SketchInputValues {
    pub fn insert_length(&mut self, id: SketchInputId<Length>, value: Length) {
        self.lengths.insert(id.key(), value);
    }

    pub fn insert_signed_length(&mut self, id: SketchInputId<SignedLength>, value: SignedLength) {
        self.signed_lengths.insert(id.key(), value);
    }

    pub fn insert_angle(&mut self, id: SketchInputId<Angle>, value: Angle) {
        self.angles.insert(id.key(), value);
    }

    pub fn insert_integer(&mut self, id: SketchInputId<Integer>, value: Integer) {
        self.integers.insert(id.key(), value);
    }
}

impl SketchValueKind for Length {
    const KIND: SketchInputKind = SketchInputKind::Length;

    fn resolve(inputs: &SketchInputValues, key: SketchInputKey) -> Option<Self> {
        inputs.lengths.get(&key).copied()
    }
}

impl SketchValueKind for SignedLength {
    const KIND: SketchInputKind = SketchInputKind::SignedLength;

    fn resolve(inputs: &SketchInputValues, key: SketchInputKey) -> Option<Self> {
        inputs.signed_lengths.get(&key).copied()
    }
}

impl SketchValueKind for Angle {
    const KIND: SketchInputKind = SketchInputKind::Angle;

    fn resolve(inputs: &SketchInputValues, key: SketchInputKey) -> Option<Self> {
        inputs.angles.get(&key).copied()
    }
}

impl SketchValueKind for Integer {
    const KIND: SketchInputKind = SketchInputKind::Integer;

    fn resolve(inputs: &SketchInputValues, key: SketchInputKey) -> Option<Self> {
        inputs.integers.get(&key).copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnresolvedSketchInput {
    pub key: SketchInputKey,
    pub expected: SketchInputKind,
}

impl fmt::Display for UnresolvedSketchInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "sketch input {} has no resolved {:?} value",
            self.key, self.expected
        )
    }
}

impl std::error::Error for UnresolvedSketchInput {}

/// An input point either reuses an earlier stable point or asks this operation
/// to create a new point at an exact sketch-plane coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum PointInput {
    Existing(SketchPointId),
    Position(SketchPoint2),
}

/// Controls whether a circular pattern's total angle is a complete periodic
/// distribution or an inclusive first-to-last extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircularPatternDistribution {
    Complete,
    Extent,
}

/// Persisted branch intent for a fillet between arbitrary analytic carriers.
///
/// The two picks identify the finite source branches which survive the edit;
/// the corner hint selects one intersection when the carriers meet more than
/// once.  Keeping all three positions in sketch-plane coordinates makes
/// replay independent of cursor pixels, tessellation, and entity ordering.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilletBranchHints {
    pub first_pick: SketchPoint2,
    pub second_pick: SketchPoint2,
    pub corner_hint: SketchPoint2,
}

/// Persisted, UI-neutral intent for every first-pass creation primitive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SketchRecipe {
    /// Exact compatibility adapter for pre-authoring-graph document profiles.
    /// It deliberately preserves geometry without claiming higher-level intent.
    LegacyImportedProfile {
        profile: PlanarProfile2,
    },
    Point {
        position: SketchPoint2,
    },
    Line {
        start: PointInput,
        end: PointInput,
    },
    CentreLine {
        start: PointInput,
        end: PointInput,
    },
    Polyline {
        vertices: Vec<PointInput>,
        closed: bool,
        construction: bool,
    },
    TwoPointRectangle {
        first_corner: PointInput,
        width: SketchValue<SignedLength>,
        height: SketchValue<SignedLength>,
    },
    CentrePointRectangle {
        center: PointInput,
        width: SketchValue<Length>,
        height: SketchValue<Length>,
    },
    CentrePointCircle {
        center: PointInput,
        radius: SketchValue<Length>,
        radial_angle: SketchValue<Angle>,
    },
    TwoPointCircle {
        first_diameter_point: PointInput,
        second_diameter_point: PointInput,
        direction: CurveDirection,
    },
    CentreStartEndArc {
        center: PointInput,
        start: PointInput,
        end: PointInput,
        direction: CurveDirection,
    },
    InnerDiameterPolygon {
        center: PointInput,
        inner_diameter: SketchValue<Length>,
        sides: SketchValue<Integer>,
        rotation: SketchValue<Angle>,
    },
    OuterDiameterPolygon {
        center: PointInput,
        outer_diameter: SketchValue<Length>,
        sides: SketchValue<Integer>,
        rotation: SketchValue<Angle>,
    },
    TwoPointSlot {
        first_cap_center: PointInput,
        second_cap_center: PointInput,
        width: SketchValue<Length>,
    },
    CentreOuterPointSlot {
        center: PointInput,
        overall_length: SketchValue<Length>,
        width: SketchValue<Length>,
        angle: SketchValue<Angle>,
    },
    RectangularPattern {
        sources: Vec<SketchEntityId>,
        columns: SketchValue<Integer>,
        rows: SketchValue<Integer>,
        column_spacing: SketchValue<SignedLength>,
        row_spacing: SketchValue<SignedLength>,
        direction: SketchValue<Angle>,
    },
    CircularPattern {
        sources: Vec<SketchEntityId>,
        center: PointInput,
        count: SketchValue<Integer>,
        total_angle: SketchValue<Angle>,
        distribution: CircularPatternDistribution,
        rotate_instances: bool,
    },
    Fillet {
        first: SketchEntityId,
        second: SketchEntityId,
        radius: SketchValue<Length>,
    },
    /// Branch-explicit fillet for every unordered line/arc/circle carrier
    /// pair.  The legacy `Fillet` recipe remains available so existing files
    /// retain byte-compatible intent and stable output identities.
    FilletWithHints {
        first: SketchEntityId,
        second: SketchEntityId,
        radius: SketchValue<Length>,
        hints: FilletBranchHints,
    },
    Chamfer {
        first: SketchEntityId,
        second: SketchEntityId,
        first_distance: SketchValue<Length>,
        second_distance: SketchValue<Length>,
    },
    /// Removes the exact adjacent span of `target` beneath `pick`. Limits are
    /// persisted in stable-ID order so replay recomputes retained analytic
    /// fragments from authoritative source curves rather than display data.
    Trim {
        target: SketchEntityId,
        limits: Vec<SketchEntityId>,
        pick: SketchPoint2,
    },
    /// A second chain holding `distance` from the first, on the side the sign
    /// chooses. Associative like the patterns: the sources are read as
    /// evaluated curves on every replay, so moving one moves the offset with
    /// it.
    ///
    /// `sources` is the chain in traversal order, with the reversal flags that
    /// make it read head to tail. Both are intent: which way round the chain is
    /// walked is what decides which side "left of travel" names.
    Offset {
        sources: Vec<crate::chain::ChainMember>,
        closed: bool,
        distance: SketchValue<SignedLength>,
    },
    FitPointSpline {
        fit_points: Vec<PointInput>,
        degree: usize,
        closed: bool,
    },
    ControlVertexSpline {
        control_points: Vec<PointInput>,
        degree: usize,
        knots: Vec<f64>,
        weights: Option<Vec<f64>>,
        closed: bool,
    },
    /// One line of text set in the bundled typeface, laid out from `anchor`
    /// along the direction `angle` with its baseline through the anchor.
    /// Every glyph contour becomes a closed loop of exact lines (see
    /// [`crate::text`]), so letters extrude like any other profile.
    Text {
        anchor: PointInput,
        content: String,
        /// Height of a capital letter.
        height: SketchValue<Length>,
        angle: SketchValue<Angle>,
    },
}

impl SketchRecipe {
    #[must_use]
    pub const fn is_compound(&self) -> bool {
        matches!(
            self,
            Self::LegacyImportedProfile { .. }
                | Self::Polyline { .. }
                | Self::TwoPointRectangle { .. }
                | Self::CentrePointRectangle { .. }
                | Self::InnerDiameterPolygon { .. }
                | Self::OuterDiameterPolygon { .. }
                | Self::TwoPointSlot { .. }
                | Self::CentreOuterPointSlot { .. }
                | Self::RectangularPattern { .. }
                | Self::CircularPattern { .. }
                | Self::Fillet { .. }
                | Self::FilletWithHints { .. }
                | Self::Chamfer { .. }
                | Self::Trim { .. }
                | Self::Offset { .. }
                | Self::Text { .. }
        )
    }

    #[must_use]
    pub const fn default_curve_role(&self) -> crate::SketchEntityRole {
        match self {
            Self::CentreLine { .. }
            | Self::Polyline {
                construction: true, ..
            } => crate::SketchEntityRole::Construction,
            Self::Point { .. } => crate::SketchEntityRole::Reference,
            _ => crate::SketchEntityRole::Profile,
        }
    }

    /// Returns stable point inputs in deterministic recipe order.
    #[must_use]
    pub fn referenced_points(&self) -> Vec<SketchPointId> {
        let mut references = Vec::new();
        let mut push = |input: &PointInput| {
            if let PointInput::Existing(id) = input {
                references.push(*id);
            }
        };
        match self {
            Self::LegacyImportedProfile { .. } | Self::Point { .. } => {}
            Self::Line { start, end } | Self::CentreLine { start, end } => {
                push(start);
                push(end);
            }
            Self::Polyline { vertices, .. } => {
                for vertex in vertices {
                    push(vertex);
                }
            }
            Self::TwoPointRectangle { first_corner, .. } => push(first_corner),
            Self::CentrePointRectangle { center, .. }
            | Self::CentrePointCircle { center, .. }
            | Self::InnerDiameterPolygon { center, .. }
            | Self::OuterDiameterPolygon { center, .. }
            | Self::CentreOuterPointSlot { center, .. } => push(center),
            Self::TwoPointCircle {
                first_diameter_point,
                second_diameter_point,
                ..
            } => {
                push(first_diameter_point);
                push(second_diameter_point);
            }
            Self::CentreStartEndArc {
                center, start, end, ..
            } => {
                push(center);
                push(start);
                push(end);
            }
            Self::TwoPointSlot {
                first_cap_center,
                second_cap_center,
                ..
            } => {
                push(first_cap_center);
                push(second_cap_center);
            }
            Self::CircularPattern { center, .. } => push(center),
            Self::Text { anchor, .. } => push(anchor),
            Self::FitPointSpline { fit_points, .. } => {
                for pt in fit_points {
                    push(pt);
                }
            }
            Self::ControlVertexSpline { control_points, .. } => {
                for pt in control_points {
                    push(pt);
                }
            }
            Self::RectangularPattern { .. }
            | Self::Fillet { .. }
            | Self::FilletWithHints { .. }
            | Self::Chamfer { .. }
            | Self::Trim { .. }
            | Self::Offset { .. } => {}
        }
        references
    }

    /// Returns all earlier curve outputs needed to deterministically replay
    /// this operation. Pattern selections are canonicalized by stable ID.
    #[must_use]
    pub fn referenced_entities(&self) -> Vec<SketchEntityId> {
        let mut entities = match self {
            Self::RectangularPattern { sources, .. } | Self::CircularPattern { sources, .. } => {
                sources.clone()
            }
            Self::Fillet { first, second, .. }
            | Self::FilletWithHints { first, second, .. }
            | Self::Chamfer { first, second, .. } => {
                vec![*first, *second]
            }
            Self::Trim { target, limits, .. } => {
                let mut entities = Vec::with_capacity(limits.len().saturating_add(1));
                entities.push(*target);
                entities.extend(limits.iter().copied());
                entities
            }
            Self::Offset { sources, .. } => sources.iter().map(|member| member.entity).collect(),
            _ => Vec::new(),
        };
        entities.sort_unstable();
        entities
    }

    /// Curve branches removed from live topology by this operation. Their
    /// stable records remain as tombstones linked to the modifier operation.
    #[must_use]
    pub fn consumed_entities(&self) -> Vec<SketchEntityId> {
        match self {
            Self::Fillet { first, second, .. }
            | Self::FilletWithHints { first, second, .. }
            | Self::Chamfer { first, second, .. } => {
                vec![*first, *second]
            }
            Self::Trim { target, .. } => vec![*target],
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub const fn is_modifier(&self) -> bool {
        matches!(
            self,
            Self::Fillet { .. }
                | Self::FilletWithHints { .. }
                | Self::Chamfer { .. }
                | Self::Trim { .. }
        )
    }

    /// Whether this recipe owns the shape of what it produces.
    ///
    /// A line is two endpoints, and a relation is free to move either one: that
    /// is what levelling a line means. A rectangle is a rectangle — move one of
    /// its corners on its own and it stops being one — so the solver moves the
    /// points of a shape-owning recipe together, as one body, and refuses a
    /// relation that would have to pull them apart.
    ///
    /// This is the recipe boundary of ADR 0026 stated over points. It already
    /// applies to curves, where a relation naming a recipe-owned curve is
    /// refused; without it over points a relation could quietly shear a
    /// rectangle into a quadrilateral.
    #[must_use]
    pub const fn owns_its_shape(&self) -> bool {
        !matches!(
            self,
            Self::Point { .. }
                | Self::Line { .. }
                | Self::CentreLine { .. }
                | Self::Polyline { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_key(raw: u64) -> SketchInputKey {
        SketchInputKey::new(raw).expect("non-zero")
    }

    #[test]
    fn typed_values_reject_non_finite_or_non_positive_inputs() {
        assert!(Length::new(0.0).is_err());
        assert!(Length::new(f64::NAN).is_err());
        assert!(SignedLength::new(f64::INFINITY).is_err());
        assert!(Angle::radians(f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn input_resolution_is_dimension_specific() {
        let key = input_key(1);
        let length_id = SketchInputId::<Length>::new(key);
        let mut values = SketchInputValues::default();
        values.insert_length(length_id, Length::new(12.0).expect("valid"));
        assert_eq!(
            SketchValue::Input(length_id).resolve(&values),
            Ok(Length::new(12.0).expect("valid"))
        );

        let angle_id = SketchInputId::<Angle>::new(key);
        let missing = SketchValue::Input(angle_id).resolve(&values);
        assert_eq!(
            missing,
            Err(UnresolvedSketchInput {
                key,
                expected: SketchInputKind::Angle,
            })
        );
    }
}

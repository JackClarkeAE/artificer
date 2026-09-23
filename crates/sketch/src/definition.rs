use crate::EvaluatedCurve2;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use artificer_protocol::{PlanarProfile2, PrecisionPolicy};
use serde::{Deserialize, Serialize};

use crate::{
    ConstraintError, ConstraintSolution, CurveOutputDraft, PointOutputDraft, SketchConstraintId,
    SketchConstraintKind, SketchConstraintRecord, SketchCurve2, SketchEntityId, SketchOperationId,
    SketchPoint2, SketchPointId, SketchRecipe, SketchRevision,
};

pub const MAX_ACTIVE_SKETCH_CURVES: usize = 1_024;
pub const MAX_ACTIVE_SKETCH_POINTS: usize = 4_096;
pub const MAX_SKETCH_OPERATIONS: usize = 1_024;
pub const MAX_CURVE_EDITS_PER_TRANSACTION: usize = 1_024;
pub const MAX_PATTERN_INSTANCES: u16 = 256;
pub const MAX_POLYGON_SIDES: u16 = 256;
pub const MIN_POLYGON_SIDES: u16 = 3;
/// Most recipe values one sketch may keep linked to document variables.
pub const MAX_SKETCH_VALUE_LINKS: usize = 4_096;
/// Longest recipe field key a value link may name, in bytes.
pub const MAX_VALUE_LINK_FIELD_BYTES: usize = 64;
/// Longest entry a value link may keep, in bytes.
pub const MAX_VALUE_LINK_TEXT_BYTES: usize = 1_024;

/// One value in a sketch that follows the document's variables.
///
/// A dimension typed as `width / 2` is worked out when it is typed, and the
/// sketch keeps the number that came out. On its own that is a copy: change
/// `width` and the sketch keeps its old size. A link is what makes it stay
/// linked. It names the value the entry was typed into and keeps the entry
/// itself, written with its units ([`crate::expression::written_entry`]) so
/// it reads the same whatever unit the document is later shown in. Whoever
/// owns the variables works the entry out again when one changes and sets
/// the value to the answer.
///
/// The sketch never evaluates a link itself; it only keeps it with the value
/// it belongs to, through edits, undo and saving, and drops it when that
/// value goes: its operation retired, or its relation removed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SketchValueLink {
    pub target: SketchValueTarget,
    pub text: String,
}

/// The value a link sets.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SketchValueTarget {
    /// One field of an operation's recipe: a rectangle's `width`, a
    /// circle's `diameter`, a line's `angle`.
    RecipeField {
        operation: SketchOperationId,
        field: String,
    },
    /// The measurement a relation holds: a dimension drawn between two
    /// points, from a point to an edge or its midpoint, or between two
    /// parallel edges.
    Relation { constraint: SketchConstraintId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "index", rename_all = "snake_case")]
pub enum PointOutputRole {
    Point,
    Start,
    End,
    Center,
    Corner(u16),
    Vertex(u16),
    RadialPoint,
    DiameterPoint(u8),
    ArcStart,
    ArcEnd,
    CapCenter(u8),
    RailEndpoint {
        rail: u8,
        endpoint: u8,
    },
    ImportedPoint(u16),
    PatternPoint {
        instance: u16,
        source: u16,
        point: u8,
    },
    /// One exact point owned by a retained Trim fragment. `point` is zero for
    /// a line start/circle centre and follows the analytic curve's canonical
    /// centre/start/end ordering for circular arcs.
    TrimPoint {
        fragment: u16,
        point: u8,
    },
    Tangency(u8),
    FilletCenter,
    /// The selected source-carrier intersection retained as an endpoint when
    /// a full circle is split into an exact circular arc by a fillet.
    FilletCorner,
    ControlPoint(u16),
    FitPoint(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "index", rename_all = "snake_case")]
pub enum CurveOutputRole {
    Curve,
    Segment(u16),
    Side(u16),
    Rail(u8),
    Cap(u8),
    Spline,
    ImportedCurve(u16),
    PatternCurve {
        instance: u16,
        source: u16,
    },
    /// One retained exact branch of the source curve after Trim removes the
    /// span under the persisted pick point.
    TrimFragment(u16),
    TrimmedSource(u8),
    CornerConnector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "output", content = "role", rename_all = "snake_case")]
pub enum OutputRole {
    Point(PointOutputRole),
    Curve(CurveOutputRole),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "output", content = "id", rename_all = "snake_case")]
pub enum SketchOutputRef {
    Point(SketchPointId),
    Curve(SketchEntityId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchOutputOwner {
    pub operation: SketchOperationId,
    pub role: PointOutputRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurveProvenance {
    pub operation: SketchOperationId,
    pub role: CurveOutputRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SketchEntityRole {
    Profile,
    Construction,
    Reference,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchPointRecord {
    pub id: SketchPointId,
    pub owner: SketchOutputOwner,
    pub evaluated_position: SketchPoint2,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchEntityRecord {
    pub id: SketchEntityId,
    pub role: SketchEntityRole,
    pub geometry: SketchCurve2,
    pub provenance: CurveProvenance,
    pub visible: bool,
    pub active: bool,
    /// Stable tombstone link for a curve retired by a later modifier.
    #[serde(default)]
    pub superseded_by: Option<SketchOperationId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchOperationRecord {
    pub id: SketchOperationId,
    pub recipe: SketchRecipe,
    #[serde(with = "output_map_serde")]
    pub outputs: BTreeMap<OutputRole, SketchOutputRef>,
    pub active: bool,
}

mod output_map_serde {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    use super::{OutputRole, SketchOutputRef};

    pub fn serialize<S>(
        outputs: &BTreeMap<OutputRole, SketchOutputRef>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        outputs.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<OutputRole, SketchOutputRef>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries = Vec::<(OutputRole, SketchOutputRef)>::deserialize(deserializer)?;
        let mut outputs = BTreeMap::new();
        for (role, output) in entries {
            if outputs.insert(role, output).is_some() {
                return Err(de::Error::custom("duplicate semantic sketch output role"));
            }
        }
        Ok(outputs)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SketchIdHighWaterMarks {
    point: u64,
    operation: u64,
    entity: u64,
    #[serde(default)]
    constraint: u64,
}

impl SketchIdHighWaterMarks {
    #[must_use]
    pub const fn point(self) -> u64 {
        self.point
    }

    #[must_use]
    pub const fn operation(self) -> u64 {
        self.operation
    }

    #[must_use]
    pub const fn entity(self) -> u64 {
        self.entity
    }

    #[must_use]
    pub const fn constraint(self) -> u64 {
        self.constraint
    }

    pub(crate) fn allocate_point(&mut self) -> Result<SketchPointId, SketchValidationError> {
        self.point = self
            .point
            .checked_add(1)
            .ok_or(SketchValidationError::IdSpaceExhausted { kind: "point" })?;
        SketchPointId::new(self.point)
            .ok_or(SketchValidationError::IdSpaceExhausted { kind: "point" })
    }

    pub(crate) fn allocate_operation(
        &mut self,
    ) -> Result<SketchOperationId, SketchValidationError> {
        self.operation = self
            .operation
            .checked_add(1)
            .ok_or(SketchValidationError::IdSpaceExhausted { kind: "operation" })?;
        SketchOperationId::new(self.operation)
            .ok_or(SketchValidationError::IdSpaceExhausted { kind: "operation" })
    }

    pub(crate) fn allocate_entity(&mut self) -> Result<SketchEntityId, SketchValidationError> {
        self.entity = self
            .entity
            .checked_add(1)
            .ok_or(SketchValidationError::IdSpaceExhausted { kind: "entity" })?;
        SketchEntityId::new(self.entity)
            .ok_or(SketchValidationError::IdSpaceExhausted { kind: "entity" })
    }

    pub(crate) fn allocate_constraint(&mut self) -> Result<SketchConstraintId, ConstraintError> {
        self.constraint = self
            .constraint
            .checked_add(1)
            .ok_or(ConstraintError::IdSpaceExhausted)?;
        SketchConstraintId::new(self.constraint).ok_or(ConstraintError::IdSpaceExhausted)
    }
}

/// Persisted exact sketch intent plus deterministic, checked evaluated caches.
/// The first entity id an arrangement gives a support curve. Authored ids
/// count up from one and never reach here.
pub const SUPPORT_CURVE_ENTITY_BASE: u64 = 1 << 62;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchDefinition {
    pub(crate) points: BTreeMap<SketchPointId, SketchPointRecord>,
    pub(crate) operations: Vec<SketchOperationRecord>,
    pub(crate) entities: BTreeMap<SketchEntityId, SketchEntityRecord>,
    #[serde(default)]
    pub(crate) constraints: BTreeMap<SketchConstraintId, SketchConstraintRecord>,
    pub(crate) allocator: SketchIdHighWaterMarks,
    pub(crate) revision: SketchRevision,
    /// The boundary of the face this sketch was drawn on — its outline and
    /// the rims of its holes — as curves the sketch can close regions
    /// against. A sketch on a face spends most of its life talking about
    /// that face, and "the face minus what I drew" is the commonest region
    /// there is. These are context, not authoring: they carry no ids the
    /// user can pick, take no part in the revision, and travel with the
    /// definition so a replay closes exactly the regions the canvas did.
    #[serde(default)]
    pub(crate) support_curves: Vec<EvaluatedCurve2>,
    /// The values that follow document variables, ordered by target, at
    /// most one per value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) value_links: Vec<SketchValueLink>,
}

impl Default for SketchDefinition {
    fn default() -> Self {
        Self::new()
    }
}

impl SketchDefinition {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            support_curves: Vec::new(),
            value_links: Vec::new(),
            points: BTreeMap::new(),
            operations: Vec::new(),
            entities: BTreeMap::new(),
            constraints: BTreeMap::new(),
            allocator: SketchIdHighWaterMarks {
                point: 0,
                operation: 0,
                entity: 0,
                constraint: 0,
            },
            revision: SketchRevision::INITIAL,
        }
    }

    /// Adapts a compiled v5 profile into an editable v6 graph without
    /// inventing rectangle, circle-gesture, or other design intent. Exact
    /// line/arc/circle uses retain deterministic profile order in one import
    /// operation and can later be dissolved or replaced explicitly.
    pub fn from_legacy_profile(
        profile: &PlanarProfile2,
        precision: PrecisionPolicy,
    ) -> Result<Self, SketchValidationError> {
        let recipe = SketchRecipe::LegacyImportedProfile {
            profile: profile.clone(),
        };
        let mut definition = Self::new();
        let evaluation = crate::evaluate_recipe(
            &definition,
            &recipe,
            &crate::SketchInputValues::default(),
            precision,
        )?;
        let operation_id = definition.allocate_operation()?;
        let operation = definition.instantiate_evaluation(
            operation_id,
            recipe,
            &evaluation.points,
            &evaluation.curves,
        )?;
        definition.push_operation(operation);
        definition.set_revision(SketchRevision::new(1));
        definition.validate(precision)?;
        Ok(definition)
    }

    #[must_use]
    pub const fn revision(&self) -> SketchRevision {
        self.revision
    }

    #[must_use]
    pub const fn high_water_marks(&self) -> SketchIdHighWaterMarks {
        self.allocator
    }

    /// Keeps allocator identities monotonic when an earlier graph snapshot is
    /// restored by local undo. Undo may restore topology and intent, but it
    /// must never make an already-published point, operation, or curve ID
    /// available for reuse.
    pub(crate) fn preserve_high_water_marks(&mut self, published: SketchIdHighWaterMarks) {
        self.allocator.point = self.allocator.point.max(published.point);
        self.allocator.operation = self.allocator.operation.max(published.operation);
        self.allocator.entity = self.allocator.entity.max(published.entity);
        self.allocator.constraint = self.allocator.constraint.max(published.constraint);
    }

    #[must_use]
    pub const fn points(&self) -> &BTreeMap<SketchPointId, SketchPointRecord> {
        &self.points
    }

    #[must_use]
    pub fn operations(&self) -> &[SketchOperationRecord] {
        &self.operations
    }

    #[must_use]
    pub const fn entities(&self) -> &BTreeMap<SketchEntityId, SketchEntityRecord> {
        &self.entities
    }

    #[must_use]
    pub const fn constraints(&self) -> &BTreeMap<SketchConstraintId, SketchConstraintRecord> {
        &self.constraints
    }

    pub fn add_constraint(
        &mut self,
        kind: SketchConstraintKind,
        precision: PrecisionPolicy,
    ) -> Result<SketchConstraintId, ConstraintError> {
        crate::validate_constraint(&kind)?;
        for point in kind.referenced_points() {
            let Some(record) = self.points.get(&point) else {
                return Err(ConstraintError::MissingPoint(point));
            };
            if !record.active {
                return Err(ConstraintError::InactivePoint(point));
            }
        }
        let id = self.allocator.allocate_constraint()?;
        self.constraints.insert(
            id,
            SketchConstraintRecord {
                id,
                kind,
                enabled: true,
            },
        );
        if let Err(error) = self.solve_constraints(precision) {
            self.constraints.remove(&id);
            return Err(error);
        }
        self.revision = self
            .revision
            .checked_next()
            .ok_or(ConstraintError::IdSpaceExhausted)?;
        Ok(id)
    }

    /// Restates one relation in place, keeping its id.
    ///
    /// A dimension drawn on the canvas is bound to the relation it shows, so
    /// retyping its value has to keep that binding: removing the relation and
    /// adding a replacement would leave the annotation pointing at something
    /// that no longer exists, and would reorder the relation against its
    /// siblings for no reason the user asked for.
    ///
    /// The swap is fail-closed in the same way as [`Self::add_constraint`]. A
    /// value the system cannot satisfy leaves the relation, the revision and
    /// the geometry exactly as they were.
    pub fn set_constraint_kind(
        &mut self,
        id: SketchConstraintId,
        kind: SketchConstraintKind,
        precision: PrecisionPolicy,
    ) -> Result<(), ConstraintError> {
        crate::validate_constraint(&kind)?;
        for point in kind.referenced_points() {
            let Some(record) = self.points.get(&point) else {
                return Err(ConstraintError::MissingPoint(point));
            };
            if !record.active {
                return Err(ConstraintError::InactivePoint(point));
            }
        }
        let Some(record) = self.constraints.get_mut(&id) else {
            return Err(ConstraintError::MissingConstraint(id));
        };
        let previous = std::mem::replace(&mut record.kind, kind);
        if let Err(error) = self.solve_constraints(precision) {
            if let Some(record) = self.constraints.get_mut(&id) {
                record.kind = previous;
            }
            return Err(error);
        }
        self.revision = self
            .revision
            .checked_next()
            .ok_or(ConstraintError::IdSpaceExhausted)?;
        Ok(())
    }

    /// Removes a relation, and with it the link its measurement followed.
    pub fn remove_constraint(&mut self, id: SketchConstraintId) -> bool {
        let removed = self.constraints.remove(&id).is_some();
        if removed {
            self.set_value_link(SketchValueTarget::Relation { constraint: id }, None);
        }
        if removed && let Some(next) = self.revision.checked_next() {
            self.revision = next;
        }
        removed
    }

    pub fn solve_constraints(
        &self,
        precision: PrecisionPolicy,
    ) -> Result<ConstraintSolution, ConstraintError> {
        self.solve_constraints_anchoring(&BTreeSet::new(), precision)
    }

    /// Solves the relation system while holding `anchored` points exactly where
    /// their recipes already put them.
    ///
    /// This is how an edit the user made by hand outranks the solver's freedom
    /// to share the movement out. Without an anchor two coincident points meet
    /// at their midpoint, so a dragged endpoint would travel half the distance
    /// the pointer did and its partner would come only half way to meet it.
    /// Anchoring the points the edit authored makes the dragged endpoint land
    /// where it was put and pulls its partner all the way onto it.
    pub fn solve_constraints_anchoring(
        &self,
        anchored: &BTreeSet<SketchPointId>,
        precision: PrecisionPolicy,
    ) -> Result<ConstraintSolution, ConstraintError> {
        let seeds = self
            .active_points()
            .map(|record| (record.id, record.evaluated_position))
            .collect();
        crate::constraints::solve(
            &seeds,
            self.constraints.values().cloned(),
            precision.linear_agreement,
            anchored,
        )
    }

    #[must_use]
    pub fn point(&self, id: SketchPointId) -> Option<&SketchPointRecord> {
        self.points.get(&id)
    }

    #[must_use]
    pub fn operation(&self, id: SketchOperationId) -> Option<&SketchOperationRecord> {
        self.operations.iter().find(|operation| operation.id == id)
    }

    #[must_use]
    pub fn entity(&self, id: SketchEntityId) -> Option<&SketchEntityRecord> {
        self.entities.get(&id)
    }

    pub fn active_points(&self) -> impl Iterator<Item = &SketchPointRecord> {
        self.points.values().filter(|point| point.active)
    }

    pub fn active_operations(&self) -> impl Iterator<Item = &SketchOperationRecord> {
        self.operations.iter().filter(|operation| operation.active)
    }

    pub fn active_entities(&self) -> impl Iterator<Item = &SketchEntityRecord> {
        self.entities.values().filter(|entity| entity.active)
    }

    /// Resolves one active ID-based cache record into coordinate geometry for
    /// analytic queries. Display tessellation is never consulted.
    pub fn evaluated_curve(
        &self,
        entity: SketchEntityId,
    ) -> Result<crate::EvaluatedCurve2, SketchValidationError> {
        let record = self
            .entities
            .get(&entity)
            .filter(|record| record.active)
            .ok_or(SketchValidationError::MissingEntity { entity })?;
        let solved = self
            .solve_constraints(PrecisionPolicy::default())
            .map_err(|_| SketchValidationError::ConstraintSystemConflict)?;
        let point = |id: SketchPointId| {
            solved
                .positions
                .get(&id)
                .copied()
                .ok_or(SketchValidationError::InactivePointReference { point: id })
        };
        Ok(match record.geometry {
            SketchCurve2::Line { start, end } => crate::EvaluatedCurve2::Line {
                start: point(start)?,
                end: point(end)?,
            },
            SketchCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => crate::EvaluatedCurve2::CircularArc {
                center: point(center)?,
                start: point(start)?,
                end: point(end)?,
                direction,
            },
            SketchCurve2::Circle {
                center,
                radius,
                direction,
            } => crate::EvaluatedCurve2::Circle {
                center: point(center)?,
                radius,
                direction,
            },
            SketchCurve2::Bspline {
                ref control_points,
                degree,
                ref knots,
                ref weights,
            } => {
                let mut evaluated_cps = Vec::with_capacity(control_points.len());
                for &cp in control_points {
                    evaluated_cps.push(point(cp)?);
                }
                crate::EvaluatedCurve2::Bspline {
                    control_points: evaluated_cps,
                    degree,
                    knots: knots.clone(),
                    weights: weights.clone(),
                }
            }
        })
    }

    /// Produces exact profile-only inputs for the planar arrangement in stable
    /// entity-ID order. Construction/reference geometry and visibility state do
    /// not alter material topology.
    /// Replaces the face boundary this sketch closes regions against.
    /// Returns whether anything changed. The revision is untouched: the
    /// boundary is the body's, not an edit of the sketch.
    pub fn set_support_curves(&mut self, curves: Vec<EvaluatedCurve2>) -> bool {
        if self.support_curves == curves {
            return false;
        }
        self.support_curves = curves;
        true
    }

    #[must_use]
    pub fn support_curves(&self) -> &[EvaluatedCurve2] {
        &self.support_curves
    }

    /// Every value that follows a document variable.
    #[must_use]
    pub fn value_links(&self) -> &[SketchValueLink] {
        &self.value_links
    }

    /// The entry one recipe field follows, if it follows one.
    #[must_use]
    pub fn value_link(&self, operation: SketchOperationId, field: &str) -> Option<&str> {
        self.value_links
            .iter()
            .find(|link| {
                matches!(&link.target, SketchValueTarget::RecipeField { operation: linked, field: named }
                    if *linked == operation && named == field)
            })
            .map(|link| link.text.as_str())
    }

    /// The entry a relation's measurement follows, if it follows one.
    #[must_use]
    pub fn relation_link(&self, constraint: SketchConstraintId) -> Option<&str> {
        self.value_links
            .iter()
            .find(|link| link.target == SketchValueTarget::Relation { constraint })
            .map(|link| link.text.as_str())
    }

    /// Links or unlinks one value, keeping the list in order. Returns
    /// whether anything changed. The revision is the caller's to advance:
    /// a link is set as part of the edit that typed it.
    pub(crate) fn set_value_link(
        &mut self,
        target: SketchValueTarget,
        text: Option<String>,
    ) -> bool {
        let found = self
            .value_links
            .binary_search_by(|link| link.target.cmp(&target));
        match (found, text) {
            (Ok(index), Some(text)) => {
                if self.value_links[index].text == text {
                    return false;
                }
                self.value_links[index].text = text;
            }
            (Ok(index), None) => {
                self.value_links.remove(index);
            }
            (Err(index), Some(text)) => self
                .value_links
                .insert(index, SketchValueLink { target, text }),
            (Err(_), None) => return false,
        }
        true
    }

    /// Drops the links whose value is gone: an operation no longer active,
    /// or a relation removed.
    pub(crate) fn prune_value_links(&mut self) {
        let active: BTreeSet<SketchOperationId> = self
            .active_operations()
            .map(|operation| operation.id)
            .collect();
        let constraints = &self.constraints;
        self.value_links.retain(|link| match &link.target {
            SketchValueTarget::RecipeField { operation, .. } => active.contains(operation),
            SketchValueTarget::Relation { constraint } => constraints.contains_key(constraint),
        });
    }

    /// Renames a variable in every link that uses it. Returns whether any
    /// link changed. Like the variable it follows, a renamed link is not an
    /// edit of the sketch, so the revision stays.
    pub fn rename_in_value_links(&mut self, from: &str, to: &str) -> bool {
        let mut changed = false;
        for link in &mut self.value_links {
            let Ok(names) = crate::expression::entry_names(&link.text) else {
                continue;
            };
            if !names.contains(from) {
                continue;
            }
            if let Ok(renamed) = crate::expression::rename_in_entry(&link.text, from, to) {
                link.text = renamed;
                changed = true;
            }
        }
        changed
    }

    /// The entity id the `index`th support curve takes in an arrangement.
    ///
    /// Support curves are not entities and have none of their own, but a
    /// fragment key names its source, and a region's signature has to be
    /// the same on every rebuild. Ids from the top of the range are what
    /// no authored entity will ever be allocated.
    #[must_use]
    pub const fn support_curve_entity(index: usize) -> SketchEntityId {
        // The base is far above zero, so the only way this is `None` is an
        // index past the top of the range, which no face has edges enough
        // to reach.
        match SketchEntityId::new(SUPPORT_CURVE_ENTITY_BASE + index as u64) {
            Some(entity) => entity,
            None => panic!("support curve index overflows the entity id range"),
        }
    }

    /// Whether an arrangement entity id names a support curve rather than
    /// an authored entity.
    #[must_use]
    pub const fn is_support_curve_entity(entity: SketchEntityId) -> bool {
        entity.get() >= SUPPORT_CURVE_ENTITY_BASE
    }

    pub fn arrangement_inputs(
        &self,
    ) -> Result<Vec<crate::ArrangementInputCurve>, SketchValidationError> {
        let support = self
            .support_curves
            .iter()
            .enumerate()
            .map(|(index, curve)| crate::ArrangementInputCurve {
                entity: Self::support_curve_entity(index),
                curve: curve.clone(),
                start_point: None,
                end_point: None,
            });
        self.active_entities()
            .filter(|entity| entity.role == SketchEntityRole::Profile)
            .map(|entity| {
                let curve = self.evaluated_curve(entity.id)?;
                let (start_point, end_point) = match entity.geometry {
                    SketchCurve2::Line { start, end }
                    | SketchCurve2::CircularArc { start, end, .. } => (Some(start), Some(end)),
                    SketchCurve2::Circle { .. } => (None, None),
                    SketchCurve2::Bspline {
                        ref control_points, ..
                    } => (
                        control_points.first().copied(),
                        control_points.last().copied(),
                    ),
                };
                Ok(crate::ArrangementInputCurve {
                    entity: entity.id,
                    curve,
                    start_point,
                    end_point,
                })
            })
            .chain(support.map(Ok))
            .collect()
    }

    pub fn validate(&self, precision: PrecisionPolicy) -> Result<(), SketchValidationError> {
        self.validate_with_inputs(&crate::SketchInputValues::default(), precision)
    }

    /// Validates structure and deterministically replays every active recipe to
    /// prove that persisted point and curve caches still match authoritative
    /// intent. Bound inputs must be supplied by the model layer.
    pub fn validate_with_inputs(
        &self,
        inputs: &crate::SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<(), SketchValidationError> {
        self.validate_structure(precision)?;
        self.verify_evaluated_caches(inputs, precision)
    }

    fn validate_structure(&self, precision: PrecisionPolicy) -> Result<(), SketchValidationError> {
        let active_point_count = self.active_points().count();
        if active_point_count > MAX_ACTIVE_SKETCH_POINTS {
            return Err(SketchValidationError::ResourceLimit {
                resource: "active_points",
                requested: active_point_count,
                limit: MAX_ACTIVE_SKETCH_POINTS,
            });
        }
        let active_entity_count = self.active_entities().count();
        if active_entity_count > MAX_ACTIVE_SKETCH_CURVES {
            return Err(SketchValidationError::ResourceLimit {
                resource: "active_curves",
                requested: active_entity_count,
                limit: MAX_ACTIVE_SKETCH_CURVES,
            });
        }
        if self.operations.len() > MAX_SKETCH_OPERATIONS {
            return Err(SketchValidationError::ResourceLimit {
                resource: "operations",
                requested: self.operations.len(),
                limit: MAX_SKETCH_OPERATIONS,
            });
        }
        if self.constraints.len() > crate::MAX_SKETCH_CONSTRAINTS {
            return Err(SketchValidationError::ResourceLimit {
                resource: "constraints",
                requested: self.constraints.len(),
                limit: crate::MAX_SKETCH_CONSTRAINTS,
            });
        }

        if self.value_links.len() > MAX_SKETCH_VALUE_LINKS {
            return Err(SketchValidationError::ResourceLimit {
                resource: "value_links",
                requested: self.value_links.len(),
                limit: MAX_SKETCH_VALUE_LINKS,
            });
        }

        let mut operation_positions = BTreeMap::new();
        let mut previous_operation_id = 0;
        for (index, operation) in self.operations.iter().enumerate() {
            if operation.id.get() <= previous_operation_id {
                return Err(SketchValidationError::NonMonotonicOperationOrder {
                    operation: operation.id,
                });
            }
            previous_operation_id = operation.id.get();
            operation_positions.insert(operation.id, index);
        }

        for (id, point) in &self.points {
            if *id != point.id {
                return Err(SketchValidationError::RecordKeyMismatch { kind: "point" });
            }
            validate_coordinate(point.evaluated_position, precision)?;
            let Some(owner_index) = operation_positions.get(&point.owner.operation) else {
                return Err(SketchValidationError::MissingOperation {
                    operation: point.owner.operation,
                });
            };
            let owner = &self.operations[*owner_index];
            if point.active && !owner.active {
                return Err(SketchValidationError::InactiveOwner {
                    operation: owner.id,
                });
            }
            if point.active
                && owner.outputs.get(&OutputRole::Point(point.owner.role))
                    != Some(&SketchOutputRef::Point(point.id))
            {
                return Err(SketchValidationError::BrokenOutputRole {
                    operation: owner.id,
                });
            }
        }

        for (id, entity) in &self.entities {
            if *id != entity.id {
                return Err(SketchValidationError::RecordKeyMismatch { kind: "entity" });
            }
            let Some(owner_index) = operation_positions.get(&entity.provenance.operation) else {
                return Err(SketchValidationError::MissingOperation {
                    operation: entity.provenance.operation,
                });
            };
            let owner = &self.operations[*owner_index];
            if entity.active && !owner.active {
                return Err(SketchValidationError::InactiveOwner {
                    operation: owner.id,
                });
            }
            if let Some(modifier) = entity.superseded_by {
                if entity.active {
                    return Err(SketchValidationError::ActiveSupersededEntity {
                        entity: entity.id,
                    });
                }
                let Some(modifier_index) = operation_positions.get(&modifier).copied() else {
                    return Err(SketchValidationError::MissingOperation {
                        operation: modifier,
                    });
                };
                if modifier_index <= *owner_index
                    || !self.operations[modifier_index].active
                    || !self.operations[modifier_index]
                        .recipe
                        .consumed_entities()
                        .contains(&entity.id)
                {
                    return Err(SketchValidationError::InvalidSupersession {
                        entity: entity.id,
                        modifier,
                    });
                }
            }
            if entity.active
                && owner
                    .outputs
                    .get(&OutputRole::Curve(entity.provenance.role))
                    != Some(&SketchOutputRef::Curve(entity.id))
            {
                return Err(SketchValidationError::BrokenOutputRole {
                    operation: owner.id,
                });
            }
            if entity.active {
                self.validate_curve(entity, *owner_index, &operation_positions, precision)?;
            }
        }

        for (id, constraint) in &self.constraints {
            if *id != constraint.id {
                return Err(SketchValidationError::RecordKeyMismatch { kind: "constraint" });
            }
            crate::validate_constraint(&constraint.kind)
                .map_err(|_| SketchValidationError::InvalidConstraint)?;
            for point in constraint.kind.referenced_points() {
                let Some(record) = self.points.get(&point) else {
                    return Err(SketchValidationError::MissingPoint { point });
                };
                if !record.active {
                    return Err(SketchValidationError::InactivePointReference { point });
                }
            }
        }
        self.solve_constraints(precision)
            .map_err(|_| SketchValidationError::ConstraintSystemConflict)?;

        for (index, link) in self.value_links.iter().enumerate() {
            let invalid = SketchValidationError::InvalidValueLink {
                target: link.target.clone(),
            };
            let ordered = index == 0 || self.value_links[index - 1].target < link.target;
            let target_exists = match &link.target {
                SketchValueTarget::RecipeField { operation, field } => {
                    operation_positions.contains_key(operation)
                        && !field.is_empty()
                        && field.len() <= MAX_VALUE_LINK_FIELD_BYTES
                }
                SketchValueTarget::Relation { constraint } => self
                    .constraints
                    .get(constraint)
                    .is_some_and(|record| record.kind.measurement().is_some()),
            };
            if !ordered || !target_exists || link.text.len() > MAX_VALUE_LINK_TEXT_BYTES {
                return Err(invalid);
            }
            match crate::expression::entry_names(&link.text) {
                Ok(names) if !names.is_empty() => {}
                _ => return Err(invalid),
            }
        }

        if self.allocator.point < self.points.keys().map(|id| id.get()).max().unwrap_or(0)
            || self.allocator.operation
                < self
                    .operations
                    .iter()
                    .map(|operation| operation.id.get())
                    .max()
                    .unwrap_or(0)
            || self.allocator.entity < self.entities.keys().map(|id| id.get()).max().unwrap_or(0)
            || self.allocator.constraint
                < self
                    .constraints
                    .keys()
                    .map(|id| id.get())
                    .max()
                    .unwrap_or(0)
        {
            return Err(SketchValidationError::HighWaterMarkRegressed);
        }
        Ok(())
    }

    fn verify_evaluated_caches(
        &self,
        inputs: &crate::SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<(), SketchValidationError> {
        let mut prefix = Self::new();
        prefix.constraints = self.constraints.clone();
        prefix.allocator.constraint = self.allocator.constraint;
        for operation in &self.operations {
            if !operation.active {
                prefix.operations.push(operation.clone());
                continue;
            }
            let evaluation = crate::evaluate_recipe(&prefix, &operation.recipe, inputs, precision)?;
            let mut expected_roles = BTreeSet::new();
            let mut point_ids = BTreeMap::new();
            for point in evaluation.points {
                let role = OutputRole::Point(point.role);
                expected_roles.insert(role);
                let Some(SketchOutputRef::Point(id)) = operation.outputs.get(&role) else {
                    return Err(SketchValidationError::EvaluatedCacheMismatch {
                        operation: operation.id,
                    });
                };
                let Some(record) = self.points.get(id) else {
                    return Err(SketchValidationError::EvaluatedCacheMismatch {
                        operation: operation.id,
                    });
                };
                if !record.active
                    || record.owner.operation != operation.id
                    || record.owner.role != point.role
                    || record.evaluated_position != point.position
                {
                    return Err(SketchValidationError::EvaluatedCacheMismatch {
                        operation: operation.id,
                    });
                }
                point_ids.insert(point.role, *id);
            }
            for curve in evaluation.curves {
                let role = OutputRole::Curve(curve.role);
                expected_roles.insert(role);
                let Some(SketchOutputRef::Curve(id)) = operation.outputs.get(&role) else {
                    return Err(SketchValidationError::EvaluatedCacheMismatch {
                        operation: operation.id,
                    });
                };
                let Some(record) = self.entities.get(id) else {
                    return Err(SketchValidationError::EvaluatedCacheMismatch {
                        operation: operation.id,
                    });
                };
                let expected_geometry = crate::instantiate_curve(curve.geometry, &point_ids)?;
                if (!record.active && record.superseded_by.is_none())
                    || record.role != curve.entity_role
                    || record.provenance.operation != operation.id
                    || record.provenance.role != curve.role
                    || record.geometry != expected_geometry
                {
                    return Err(SketchValidationError::EvaluatedCacheMismatch {
                        operation: operation.id,
                    });
                }
            }
            if operation.outputs.keys().copied().collect::<BTreeSet<_>>() != expected_roles {
                return Err(SketchValidationError::EvaluatedCacheMismatch {
                    operation: operation.id,
                });
            }
            for output in operation.outputs.values() {
                match output {
                    SketchOutputRef::Point(id) => {
                        prefix.points.insert(*id, self.points[id].clone());
                    }
                    SketchOutputRef::Curve(id) => {
                        let mut record = self.entities[id].clone();
                        record.active = true;
                        record.superseded_by = None;
                        prefix.entities.insert(*id, record);
                    }
                }
            }
            prefix.operations.push(operation.clone());
            for source in operation.recipe.consumed_entities() {
                let record = prefix
                    .entities
                    .get_mut(&source)
                    .ok_or(SketchValidationError::MissingEntity { entity: source })?;
                record.active = false;
                record.superseded_by = Some(operation.id);
            }
        }
        Ok(())
    }

    fn validate_curve(
        &self,
        entity: &SketchEntityRecord,
        owner_index: usize,
        operation_positions: &BTreeMap<SketchOperationId, usize>,
        precision: PrecisionPolicy,
    ) -> Result<(), SketchValidationError> {
        let validate_reference = |point_id: SketchPointId| {
            let point = self
                .points
                .get(&point_id)
                .ok_or(SketchValidationError::MissingPoint { point: point_id })?;
            if !point.active {
                return Err(SketchValidationError::InactivePointReference { point: point_id });
            }
            let point_owner_index = operation_positions
                .get(&point.owner.operation)
                .copied()
                .ok_or(SketchValidationError::MissingOperation {
                    operation: point.owner.operation,
                })?;
            if point_owner_index > owner_index {
                return Err(SketchValidationError::ForwardPointReference {
                    point: point_id,
                    operation: entity.provenance.operation,
                });
            }
            Ok(point.evaluated_position)
        };

        match entity.geometry {
            SketchCurve2::Line { start, end } => {
                let start = validate_reference(start)?;
                let end = validate_reference(end)?;
                if distance(start, end) < precision.min_feature_size {
                    return Err(SketchValidationError::FeatureTooSmall {
                        operation: entity.provenance.operation,
                    });
                }
            }
            SketchCurve2::CircularArc {
                center, start, end, ..
            } => {
                let center = validate_reference(center)?;
                let start = validate_reference(start)?;
                let end = validate_reference(end)?;
                let start_radius = distance(center, start);
                let end_radius = distance(center, end);
                if start_radius < precision.min_feature_size
                    || end_radius < precision.min_feature_size
                    || distance(start, end) < precision.min_feature_size
                {
                    return Err(SketchValidationError::FeatureTooSmall {
                        operation: entity.provenance.operation,
                    });
                }
                if (start_radius - end_radius).abs() > precision.linear_agreement {
                    return Err(SketchValidationError::ArcRadiusMismatch {
                        operation: entity.provenance.operation,
                    });
                }
            }
            SketchCurve2::Circle { center, radius, .. } => {
                let _ = validate_reference(center)?;
                if !radius.is_finite() {
                    return Err(SketchValidationError::NonFiniteValue);
                }
                if radius < precision.min_feature_size {
                    return Err(SketchValidationError::FeatureTooSmall {
                        operation: entity.provenance.operation,
                    });
                }
            }
            SketchCurve2::Bspline {
                ref control_points,
                degree,
                ref knots,
                ref weights,
            } => {
                if control_points.len() <= degree || degree == 0 {
                    return Err(SketchValidationError::FeatureTooSmall {
                        operation: entity.provenance.operation,
                    });
                }
                for &cp in control_points {
                    let _ = validate_reference(cp)?;
                }
                if knots.len() != control_points.len() + degree + 1 {
                    return Err(SketchValidationError::FeatureTooSmall {
                        operation: entity.provenance.operation,
                    });
                }
                if let Some(w) = weights
                    && (w.len() != control_points.len()
                        || w.iter().any(|v| !v.is_finite() || *v <= 0.0))
                {
                    return Err(SketchValidationError::NonFiniteValue);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn allocate_point(&mut self) -> Result<SketchPointId, SketchValidationError> {
        self.allocator.allocate_point()
    }

    pub(crate) fn allocate_operation(
        &mut self,
    ) -> Result<SketchOperationId, SketchValidationError> {
        self.allocator.allocate_operation()
    }

    pub(crate) fn allocate_entity(&mut self) -> Result<SketchEntityId, SketchValidationError> {
        self.allocator.allocate_entity()
    }

    pub(crate) fn insert_point(&mut self, point: SketchPointRecord) {
        self.points.insert(point.id, point);
    }

    pub(crate) fn insert_entity(&mut self, entity: SketchEntityRecord) {
        self.entities.insert(entity.id, entity);
    }

    pub(crate) fn push_operation(&mut self, operation: SketchOperationRecord) {
        self.operations.push(operation);
    }

    pub(crate) fn point_mut(&mut self, id: SketchPointId) -> Option<&mut SketchPointRecord> {
        self.points.get_mut(&id)
    }

    pub(crate) fn entity_mut(&mut self, id: SketchEntityId) -> Option<&mut SketchEntityRecord> {
        self.entities.get_mut(&id)
    }

    pub(crate) fn set_revision(&mut self, revision: SketchRevision) {
        self.revision = revision;
    }

    pub(crate) fn instantiate_evaluation(
        &mut self,
        operation_id: SketchOperationId,
        recipe: SketchRecipe,
        points: &[PointOutputDraft],
        curves: &[CurveOutputDraft],
    ) -> Result<SketchOperationRecord, SketchValidationError> {
        crate::instantiate_evaluation(self, operation_id, recipe, points, curves)
    }
}

fn validate_coordinate(
    point: SketchPoint2,
    precision: PrecisionPolicy,
) -> Result<(), SketchValidationError> {
    if !point.is_finite() {
        return Err(SketchValidationError::NonFiniteValue);
    }
    if point.u.abs() > precision.max_abs_coordinate || point.v.abs() > precision.max_abs_coordinate
    {
        return Err(SketchValidationError::CoordinateOutOfBounds {
            max_abs_coordinate: precision.max_abs_coordinate,
        });
    }
    Ok(())
}

fn distance(first: SketchPoint2, second: SketchPoint2) -> f64 {
    (second.u - first.u).hypot(second.v - first.v)
}

#[derive(Clone, Debug, PartialEq)]
pub enum SketchValidationError {
    NonFiniteValue,
    CoordinateOutOfBounds {
        max_abs_coordinate: f64,
    },
    FeatureTooSmall {
        operation: SketchOperationId,
    },
    ArcRadiusMismatch {
        operation: SketchOperationId,
    },
    PolygonSideCount {
        count: u16,
    },
    InvalidSlotDimensions,
    /// The text recipe could not be set: empty, a glyph the bundled
    /// typeface lacks, or more outline vertices than a sketch may hold.
    TextUnavailable {
        reason: crate::text::TextOutlineError,
    },
    MissingPoint {
        point: SketchPointId,
    },
    MissingEntity {
        entity: SketchEntityId,
    },
    InactivePointReference {
        point: SketchPointId,
    },
    ForwardPointReference {
        point: SketchPointId,
        operation: SketchOperationId,
    },
    MissingOperation {
        operation: SketchOperationId,
    },
    InactiveOwner {
        operation: SketchOperationId,
    },
    MissingInput {
        key: crate::SketchInputKey,
        expected: crate::SketchInputKind,
    },
    ResourceLimit {
        resource: &'static str,
        requested: usize,
        limit: usize,
    },
    ArithmeticOverflow,
    IdSpaceExhausted {
        kind: &'static str,
    },
    NonMonotonicOperationOrder {
        operation: SketchOperationId,
    },
    RecordKeyMismatch {
        kind: &'static str,
    },
    BrokenOutputRole {
        operation: SketchOperationId,
    },
    HighWaterMarkRegressed,
    DuplicateOutputRole,
    EvaluatedCacheMismatch {
        operation: SketchOperationId,
    },
    PatternCount {
        count: u16,
        minimum: u16,
    },
    EmptyEntitySelection,
    DuplicateEntitySelection {
        entity: SketchEntityId,
    },
    UnsupportedPatternSource {
        entity: SketchEntityId,
    },
    InvalidCornerSelection,
    CornerDistanceTooLarge,
    FilletHintOffSource {
        entity: SketchEntityId,
    },
    FilletNoBoundedSolution,
    FilletAmbiguousSolution,
    FilletTangencyFailure,
    InvalidTrimSelection,
    TrimRoleMismatch {
        target: SketchEntityId,
        limit: SketchEntityId,
    },
    ActiveSupersededEntity {
        entity: SketchEntityId,
    },
    InvalidSupersession {
        entity: SketchEntityId,
        modifier: SketchOperationId,
    },
    InvalidConstraint,
    ConstraintSystemConflict,
    /// A value link out of order, on a value the sketch does not have, or
    /// whose entry does not read or names no variable.
    InvalidValueLink {
        target: SketchValueTarget,
    },
}

impl fmt::Display for SketchValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteValue => formatter.write_str("sketch value must be finite"),
            Self::CoordinateOutOfBounds { max_abs_coordinate } => write!(
                formatter,
                "sketch coordinate exceeds the ±{max_abs_coordinate} envelope"
            ),
            Self::FeatureTooSmall { operation } => {
                write!(
                    formatter,
                    "operation {operation} creates a feature below the minimum size"
                )
            }
            Self::ArcRadiusMismatch { operation } => {
                write!(
                    formatter,
                    "operation {operation} has inconsistent arc radii"
                )
            }
            Self::PolygonSideCount { count } => write!(
                formatter,
                "polygon side count {count} is outside {MIN_POLYGON_SIDES}..={MAX_POLYGON_SIDES}"
            ),
            Self::InvalidSlotDimensions => {
                formatter.write_str("slot width must be positive and smaller than overall length")
            }
            Self::TextUnavailable { reason } => write!(formatter, "text cannot be set: {reason}"),
            Self::MissingPoint { point } => write!(formatter, "point {point} does not exist"),
            Self::MissingEntity { entity } => {
                write!(formatter, "entity {entity} does not exist or is retired")
            }
            Self::InactivePointReference { point } => {
                write!(formatter, "point {point} has been retired")
            }
            Self::ForwardPointReference { point, operation } => write!(
                formatter,
                "operation {operation} references later point {point}"
            ),
            Self::MissingOperation { operation } => {
                write!(formatter, "operation {operation} does not exist")
            }
            Self::InactiveOwner { operation } => {
                write!(
                    formatter,
                    "active output belongs to retired operation {operation}"
                )
            }
            Self::MissingInput { key, expected } => {
                write!(formatter, "input {key} has no resolved {expected:?} value")
            }
            Self::ResourceLimit {
                resource,
                requested,
                limit,
            } => write!(
                formatter,
                "{resource} requests {requested} items, exceeding the limit of {limit}"
            ),
            Self::ArithmeticOverflow => formatter.write_str("checked sketch arithmetic overflowed"),
            Self::IdSpaceExhausted { kind } => write!(formatter, "{kind} ID space is exhausted"),
            Self::NonMonotonicOperationOrder { operation } => write!(
                formatter,
                "operation {operation} violates monotonic operation ordering"
            ),
            Self::RecordKeyMismatch { kind } => {
                write!(formatter, "{kind} record ID does not match its map key")
            }
            Self::BrokenOutputRole { operation } => {
                write!(
                    formatter,
                    "operation {operation} has a broken semantic output map"
                )
            }
            Self::HighWaterMarkRegressed => {
                formatter.write_str("stable ID high-water marks regressed below persisted records")
            }
            Self::DuplicateOutputRole => {
                formatter.write_str("operation evaluation produced a duplicate semantic role")
            }
            Self::EvaluatedCacheMismatch { operation } => write!(
                formatter,
                "operation {operation} evaluated cache does not match its authoritative recipe"
            ),
            Self::PatternCount { count, minimum } => write!(
                formatter,
                "pattern count {count} is outside {minimum}..={MAX_PATTERN_INSTANCES}"
            ),
            Self::EmptyEntitySelection => {
                formatter.write_str("sketch edit requires at least one source entity")
            }
            Self::DuplicateEntitySelection { entity } => {
                write!(
                    formatter,
                    "source entity {entity} is selected more than once"
                )
            }
            Self::UnsupportedPatternSource { entity } => {
                write!(
                    formatter,
                    "entity {entity} cannot be used by this sketch edit"
                )
            }
            Self::InvalidCornerSelection => formatter
                .write_str("fillet and chamfer require two distinct, connected line segments"),
            Self::CornerDistanceTooLarge => formatter.write_str(
                "fillet or chamfer trim distance does not fit on both selected segments",
            ),
            Self::FilletHintOffSource { entity } => write!(
                formatter,
                "fillet branch hint does not lie on source entity {entity}"
            ),
            Self::FilletNoBoundedSolution => formatter.write_str(
                "fillet has no finite, bounded, no-extension solution for the selected branches",
            ),
            Self::FilletAmbiguousSolution => formatter
                .write_str("fillet branch hints do not select one unique analytic solution"),
            Self::FilletTangencyFailure => {
                formatter.write_str("fillet candidate failed the exact radius or tangency proof")
            }
            Self::InvalidTrimSelection => {
                formatter.write_str("trim does not resolve to one exact adjacent span")
            }
            Self::TrimRoleMismatch { target, limit } => write!(
                formatter,
                "trim target {target} and limit {limit} have incompatible geometry roles"
            ),
            Self::ActiveSupersededEntity { entity } => {
                write!(formatter, "superseded entity {entity} cannot remain active")
            }
            Self::InvalidSupersession { entity, modifier } => write!(
                formatter,
                "entity {entity} has an invalid supersession link to operation {modifier}"
            ),
            Self::InvalidConstraint => formatter.write_str("sketch constraint is invalid"),
            Self::ConstraintSystemConflict => {
                formatter.write_str("sketch constraint system is conflicting")
            }
            Self::InvalidValueLink { target } => match target {
                SketchValueTarget::RecipeField { operation, field } => write!(
                    formatter,
                    "the {field} of operation {operation} is linked to an entry that cannot be kept"
                ),
                SketchValueTarget::Relation { constraint } => write!(
                    formatter,
                    "the measurement of relation {constraint} is linked to an entry that cannot be kept"
                ),
            },
        }
    }
}

impl std::error::Error for SketchValidationError {}

impl From<crate::UnresolvedSketchInput> for SketchValidationError {
    fn from(error: crate::UnresolvedSketchInput) -> Self {
        Self::MissingInput {
            key: error.key,
            expected: error.expected,
        }
    }
}

/// IDs that remain persisted as inactive tombstones after an edit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SketchTombstones {
    pub points: BTreeSet<SketchPointId>,
    pub operations: BTreeSet<SketchOperationId>,
    pub entities: BTreeSet<SketchEntityId>,
}

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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchDefinition {
    pub(crate) points: BTreeMap<SketchPointId, SketchPointRecord>,
    pub(crate) operations: Vec<SketchOperationRecord>,
    pub(crate) entities: BTreeMap<SketchEntityId, SketchEntityRecord>,
    #[serde(default)]
    pub(crate) constraints: BTreeMap<SketchConstraintId, SketchConstraintRecord>,
    pub(crate) allocator: SketchIdHighWaterMarks,
    pub(crate) revision: SketchRevision,
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
        // A relation over points one shape owns has nothing to solve: the
        // solver may translate that shape but never pull it apart, and the
        // shape's own dimensions are where its size is set. Refusing by name
        // is what keeps a rectangle a rectangle.
        let referenced = kind.referenced_points();
        let groups = self.rigid_point_groups();
        if referenced.len() > 1
            && referenced
                .iter()
                .all(|point| groups.share_a_body(referenced[0], *point))
        {
            return Err(ConstraintError::WithinOneShape);
        }
        let id = self.allocator.allocate_constraint()?;
        self.constraints.insert(
            id,
            SketchConstraintRecord {
                id,
                kind,
                enabled: true,
                label_offset: None,
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

    /// Retypes the value a dimension holds, keeping its identity and its
    /// operands.
    ///
    /// The new value is refused exactly as a new relation is: a system that
    /// will not converge leaves the old value in place, so a typed number
    /// either takes or is explained, never half-applies.
    pub fn set_constraint_value(
        &mut self,
        id: SketchConstraintId,
        value: f64,
        precision: PrecisionPolicy,
    ) -> Result<(), ConstraintError> {
        let Some(record) = self.constraints.get(&id) else {
            return Err(ConstraintError::MissingConstraint(id));
        };
        let Some(kind) = record.kind.with_value(value) else {
            return Err(ConstraintError::NotADimension);
        };
        crate::validate_constraint(&kind)?;
        let previous = std::mem::replace(
            &mut self.constraints.get_mut(&id).expect("checked").kind,
            kind,
        );
        if let Err(error) = self.solve_constraints(precision) {
            self.constraints.get_mut(&id).expect("checked").kind = previous;
            return Err(error);
        }
        self.revision = self
            .revision
            .checked_next()
            .ok_or(ConstraintError::IdSpaceExhausted)?;
        Ok(())
    }

    /// Moves where a dimension's value is drawn, as an offset in sketch units
    /// from the middle of what it measures.
    ///
    /// Deliberately outside the transaction machinery, and deliberately not a
    /// new revision. Dragging a label changes the drawing, not the geometry:
    /// advancing the revision here would mark every feature built on this
    /// sketch stale, so tidying an annotation would ask for a rebuild. The
    /// offset still travels with the document, because where a dimension sits
    /// is part of the drawing.
    ///
    /// Answers whether anything changed. A non-finite offset is refused rather
    /// than stored, so a label can never be moved somewhere unpaintable.
    pub fn set_constraint_label_offset(
        &mut self,
        id: SketchConstraintId,
        offset: Option<SketchPoint2>,
    ) -> bool {
        if offset.is_some_and(|offset| !offset.is_finite()) {
            return false;
        }
        let Some(record) = self.constraints.get_mut(&id) else {
            return false;
        };
        if record.label_offset == offset {
            return false;
        }
        record.label_offset = offset;
        true
    }

    pub fn remove_constraint(&mut self, id: SketchConstraintId) -> bool {
        let removed = self.constraints.remove(&id).is_some();
        if removed && let Some(next) = self.revision.checked_next() {
            self.revision = next;
        }
        removed
    }

    pub fn solve_constraints(
        &self,
        precision: PrecisionPolicy,
    ) -> Result<ConstraintSolution, ConstraintError> {
        let seeds = self
            .active_points()
            .map(|record| (record.id, record.evaluated_position))
            .collect();
        crate::constraints::solve(
            &seeds,
            self.constraints.values().cloned(),
            &self.rigid_point_groups(),
            precision.linear_agreement,
        )
    }

    /// Which points the solver must move together.
    ///
    /// One group per shape-owning operation: a rectangle's corners and centre
    /// travel as a rectangle, a circle's centre and rim as a circle. A line's
    /// endpoints stay independent, which is what lets a relation level one.
    #[must_use]
    pub fn rigid_point_groups(&self) -> crate::constraints::RigidPointGroups {
        let mut groups = crate::constraints::RigidPointGroups::default();
        for operation in self.active_operations() {
            if !operation.recipe.owns_its_shape() {
                continue;
            }
            groups.insert_group(
                self.active_points()
                    .filter(|record| record.owner.operation == operation.id)
                    .map(|record| record.id),
            );
        }
        groups
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
        let solved = self
            .solve_constraints(PrecisionPolicy::default())
            .map_err(|_| SketchValidationError::ConstraintSystemConflict)?;
        self.evaluated_curve_from(entity, &solved)
    }

    /// Every active curve of the sketch, in stable entity order, from one
    /// constraint solve.
    ///
    /// [`Self::evaluated_curve`] solves the whole constraint system to answer
    /// for one curve, which is the right shape for a single query and the wrong
    /// one for a walk over the sketch: this exists so a caller that wants many
    /// curves pays for the solve once.
    pub fn evaluated_curves(
        &self,
    ) -> Result<Vec<(SketchEntityId, crate::EvaluatedCurve2)>, SketchValidationError> {
        let solved = self
            .solve_constraints(PrecisionPolicy::default())
            .map_err(|_| SketchValidationError::ConstraintSystemConflict)?;
        let mut entities = self
            .active_entities()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        entities.sort_unstable();
        entities
            .into_iter()
            .map(|entity| Ok((entity, self.evaluated_curve_from(entity, &solved)?)))
            .collect()
    }

    fn evaluated_curve_from(
        &self,
        entity: SketchEntityId,
        solved: &ConstraintSolution,
    ) -> Result<crate::EvaluatedCurve2, SketchValidationError> {
        let record = self
            .entities
            .get(&entity)
            .filter(|record| record.active)
            .ok_or(SketchValidationError::MissingEntity { entity })?;
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
    pub fn arrangement_inputs(
        &self,
    ) -> Result<Vec<crate::ArrangementInputCurve>, SketchValidationError> {
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
    /// The offset could not be produced. The reason names the curve or the
    /// corner, because "the offset failed" is not something a user can act on.
    OffsetRefused {
        reason: crate::offset::OffsetError,
    },
    /// A chain could not be walked from the picked curve.
    ChainRefused {
        reason: crate::chain::ChainError,
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
            Self::OffsetRefused { reason } => write!(formatter, "offset refused: {reason}"),
            Self::ChainRefused { reason } => write!(formatter, "chain refused: {reason}"),
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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use artificer_protocol::PrecisionPolicy;

use crate::{
    ConstraintError, CurveProvenance, OutputRole, PointOutputRole, PrimitiveEvaluation,
    SketchConstraintId, SketchConstraintKind, SketchDefinition, SketchEntityId, SketchEntityRecord,
    SketchInputValues, SketchOperationId, SketchOutputOwner, SketchOutputRef, SketchPoint2,
    SketchPointId, SketchPointRecord, SketchRecipe, SketchRevision, SketchValidationError,
    evaluate_recipe, instantiate_curve,
};

/// Visible confirmation path used to publish an atomic sketch edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmationSource {
    GreenTick,
    BareEnter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetirementPolicy {
    RejectDependents,
    CascadeDependents,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SketchImpactReport {
    pub inserted_operations: BTreeSet<SketchOperationId>,
    pub changed_operations: BTreeSet<SketchOperationId>,
    pub retired_operations: BTreeSet<SketchOperationId>,
    pub inserted_points: BTreeSet<SketchPointId>,
    pub changed_points: BTreeSet<SketchPointId>,
    pub retired_points: BTreeSet<SketchPointId>,
    pub inserted_entities: BTreeSet<SketchEntityId>,
    pub changed_entities: BTreeSet<SketchEntityId>,
    pub retired_entities: BTreeSet<SketchEntityId>,
    pub superseded_entities: BTreeSet<SketchEntityId>,
    pub restored_entities: BTreeSet<SketchEntityId>,
    pub profile_changed: bool,
    pub construction_changed: bool,
    pub visibility_changed: bool,
}

/// Fully evaluated candidate overlay. It owns a private candidate definition;
/// none of its provisional allocations affect the live sketch until confirm.
#[derive(Clone, Debug)]
pub struct SketchTransaction {
    expected_revision: SketchRevision,
    label: String,
    candidate: SketchDefinition,
    impact: SketchImpactReport,
    inputs: SketchInputValues,
    precision: PrecisionPolicy,
}

impl SketchTransaction {
    #[must_use]
    pub const fn expected_revision(&self) -> SketchRevision {
        self.expected_revision
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn preview(&self) -> &SketchDefinition {
        &self.candidate
    }

    #[must_use]
    pub const fn impact(&self) -> &SketchImpactReport {
        &self.impact
    }

    /// Appends one branch-replacing modifier to this transaction's current
    /// candidate. The complete batch retains the revision observed when the
    /// first operation was staged and publishes only one successor revision.
    ///
    /// Evaluation is copy-on-success: an invalid append leaves this
    /// transaction, including its preview, provisional identifiers, and
    /// impact report, unchanged and available for correction or commit.
    pub fn append_modifier(&mut self, recipe: SketchRecipe) -> Result<(), SketchTransactionError> {
        if !recipe.is_modifier() {
            return Err(SketchTransactionError::NotAModifier);
        }

        // `stage_modifier_with_inputs` normally advances the definition it is
        // called on. The candidate already represents the one unpublished
        // successor revision, so stage from a private revision-neutral view of
        // that candidate. Its geometry and high-water marks are unchanged.
        let mut append_base = self.candidate.clone();
        append_base.set_revision(self.expected_revision);
        let appended = append_base.stage_modifier_with_inputs(
            recipe,
            self.label.clone(),
            &self.inputs,
            self.precision,
        )?;

        let mut impact = self.impact.clone();
        merge_impact(&mut impact, appended.impact);
        normalize_composed_impact(&appended.candidate, &mut impact);

        self.candidate = appended.candidate;
        self.impact = impact;
        debug_assert_eq!(
            self.candidate.revision(),
            next_revision(self.expected_revision)
                .expect("staging already proved revision capacity")
        );
        Ok(())
    }

    /// Adds relations to this transaction's candidate, so geometry and the
    /// relations it implies arrive, and are undone, together.
    ///
    /// A stroke that ends on another stroke's endpoint is one act as the user
    /// experiences it, and splitting it into "add the line" followed by "add
    /// the coincidence" would make them undo twice to take it back. Evaluation
    /// is copy-on-success exactly as it is for an appended modifier: a relation
    /// the system cannot satisfy leaves this transaction untouched.
    pub fn append_constraints(
        &mut self,
        kinds: Vec<SketchConstraintKind>,
    ) -> Result<(), SketchTransactionError> {
        if kinds.is_empty() {
            return Err(SketchTransactionError::NoChange);
        }
        let before = self
            .candidate
            .solve_constraints(self.precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;
        let mut candidate = self.candidate.clone();
        for kind in kinds {
            candidate
                .add_constraint(kind, self.precision)
                .map_err(SketchTransactionError::ConstraintRejected)?;
        }
        let after = candidate
            .solve_constraints(self.precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;

        // Each `add_constraint` advances the revision it is applied to; the
        // batch still publishes the one successor this transaction promised.
        candidate.set_revision(next_revision(self.expected_revision)?);
        candidate.validate_with_inputs(&self.inputs, self.precision)?;

        let mut impact = self.impact.clone();
        impact.profile_changed = true;
        for (point, position) in &after.positions {
            let moved = before
                .positions
                .get(point)
                .is_none_or(|previous| !positions_agree(*previous, *position, self.precision));
            if moved {
                impact.changed_points.insert(*point);
                for (entity, record) in candidate.entities() {
                    if record.active && record.geometry.referenced_points().contains(point) {
                        impact.changed_entities.insert(*entity);
                    }
                }
            }
        }
        normalize_composed_impact(&candidate, &mut impact);
        self.candidate = candidate;
        self.impact = impact;
        Ok(())
    }

    /// Replaces a further operation's recipe inside this same unpublished edit.
    ///
    /// Pulling a relation's follower rewrites the follower's own authored
    /// intent, and that has to land in the edit that moved it rather than
    /// trailing behind as a second revision the user can undo separately.
    fn append_replace(
        &mut self,
        operation: SketchOperationId,
        recipe: SketchRecipe,
    ) -> Result<(), SketchTransactionError> {
        let mut append_base = self.candidate.clone();
        append_base.set_revision(self.expected_revision);
        let appended = append_base.stage_replace(
            operation,
            recipe,
            self.label.clone(),
            &self.inputs,
            self.precision,
        )?;

        let mut impact = self.impact.clone();
        merge_impact(&mut impact, appended.impact);
        normalize_composed_impact(&appended.candidate, &mut impact);

        self.candidate = appended.candidate;
        self.impact = impact;
        Ok(())
    }

    /// Convenience seam for accumulating exact Trim modifiers behind one
    /// visible confirmation. Limits are canonicalized exactly as they are for
    /// the first staged Trim, and every append evaluates against the preceding
    /// candidate rather than the live definition.
    pub fn append_trim(
        &mut self,
        target: SketchEntityId,
        mut limits: Vec<SketchEntityId>,
        pick: SketchPoint2,
    ) -> Result<(), SketchTransactionError> {
        limits.sort_unstable();
        limits.dedup();
        self.append_modifier(SketchRecipe::Trim {
            target,
            limits,
            pick,
        })
    }

    /// Drops the candidate without touching the live definition.
    #[must_use]
    pub fn cancel(self) -> SketchCancellation {
        SketchCancellation {
            unchanged_revision: self.expected_revision,
            label: self.label,
        }
    }
}

/// Whether two solved positions agree within the linear tolerance, so a
/// relation the sketch already satisfies reports no movement.
fn positions_agree(left: SketchPoint2, right: SketchPoint2, precision: PrecisionPolicy) -> bool {
    (left.u - right.u).hypot(left.v - right.v) <= precision.linear_agreement
}

/// How many rings of followers one edit may drag behind it.
///
/// Each pass settles the operations joined to whatever the previous pass
/// settled, so a chain of coincident lines straightens out from the edit
/// outwards. The bound is here so that a sketch whose relations cannot all be
/// honoured stops rather than trading positions back and forth.
const MAX_FOLLOWER_PASSES: usize = 8;

/// The active points one operation authored, which an edit to that operation
/// is entitled to hold still.
fn points_authored_by(
    definition: &SketchDefinition,
    operation: SketchOperationId,
) -> BTreeSet<SketchPointId> {
    definition
        .active_points()
        .filter(|record| record.owner.operation == operation)
        .map(|record| record.id)
        .collect()
}

/// Writes the points an edit moved back into the recipes that author them, so
/// the sketch satisfies its relations outright rather than by being solved.
///
/// `anchored` is what the edit is entitled to hold: its own points. Each pass
/// solves with that set held, writes every point the solver had to move into
/// its owning recipe, and adds those points to the held set so the next pass
/// works outwards from them. A follower whose recipe cannot state the point as
/// a literal (see [`SketchRecipe::set_authored_point`]), or whose replacement
/// does not replay, is left where it is and the ordinary solve goes on sharing
/// the movement for that one relation.
fn pull_followers(
    mut transaction: SketchTransaction,
    mut anchored: BTreeSet<SketchPointId>,
    precision: PrecisionPolicy,
) -> SketchTransaction {
    for _ in 0..MAX_FOLLOWER_PASSES {
        let Ok(solution) = transaction
            .candidate
            .solve_constraints_anchoring(&anchored, precision)
        else {
            break;
        };
        let mut pulls: BTreeMap<SketchOperationId, Vec<(PointOutputRole, SketchPoint2)>> =
            BTreeMap::new();
        for (point, position) in &solution.positions {
            if anchored.contains(point) {
                continue;
            }
            let Some(record) = transaction.candidate.point(*point) else {
                continue;
            };
            if !record.active || positions_agree(record.evaluated_position, *position, precision) {
                continue;
            }
            pulls
                .entry(record.owner.operation)
                .or_default()
                .push((record.owner.role, *position));
        }
        let mut pulled = false;
        for (follower, placements) in pulls {
            let Some(record) = transaction
                .candidate
                .operation(follower)
                .filter(|record| record.active)
            else {
                continue;
            };
            let mut recipe = record.recipe.clone();
            let mut rewritten = false;
            for (role, position) in placements {
                rewritten |= recipe.set_authored_point(role, position);
            }
            if !rewritten {
                continue;
            }
            // A follower whose replacement does not replay is left alone: the
            // relation is worth less than the geometry already there.
            let mut attempt = transaction.clone();
            if attempt.append_replace(follower, recipe).is_ok() {
                transaction = attempt;
                anchored.extend(points_authored_by(&transaction.candidate, follower));
                pulled = true;
            }
        }
        if !pulled {
            break;
        }
    }
    transaction
}

fn merge_impact(impact: &mut SketchImpactReport, appended: SketchImpactReport) {
    impact
        .inserted_operations
        .extend(appended.inserted_operations);
    impact
        .changed_operations
        .extend(appended.changed_operations);
    impact
        .retired_operations
        .extend(appended.retired_operations);
    impact.inserted_points.extend(appended.inserted_points);
    impact.changed_points.extend(appended.changed_points);
    impact.retired_points.extend(appended.retired_points);
    impact.inserted_entities.extend(appended.inserted_entities);
    impact.changed_entities.extend(appended.changed_entities);
    impact.retired_entities.extend(appended.retired_entities);
    impact
        .superseded_entities
        .extend(appended.superseded_entities);
    impact.restored_entities.extend(appended.restored_entities);
    impact.profile_changed |= appended.profile_changed;
    impact.construction_changed |= appended.construction_changed;
    impact.visibility_changed |= appended.visibility_changed;
}

fn normalize_composed_impact(candidate: &SketchDefinition, impact: &mut SketchImpactReport) {
    // Outputs allocated earlier in this unpublished batch can be consumed by
    // a later modifier. They never existed in the live sketch, so the final
    // net impact must neither advertise them as active insertions nor as
    // changes/retirements of pre-existing entities. Their inactive records
    // remain in the candidate operation graph for deterministic replay.
    let provisional_entities = impact.inserted_entities.clone();
    for entity in &provisional_entities {
        impact.changed_entities.remove(entity);
        impact.retired_entities.remove(entity);
        impact.superseded_entities.remove(entity);
        impact.restored_entities.remove(entity);
    }
    impact.inserted_entities.retain(|entity| {
        candidate
            .entity(*entity)
            .is_some_and(|record| record.active)
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SketchCancellation {
    pub unchanged_revision: SketchRevision,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SketchCommit {
    pub revision: SketchRevision,
    pub confirmation: ConfirmationSource,
    pub label: String,
    pub impact: SketchImpactReport,
}

impl SketchDefinition {
    /// Stages one primitive using literal recipe values and the default product
    /// precision policy.
    pub fn stage(
        &self,
        recipe: SketchRecipe,
        label: impl Into<String>,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        self.stage_with_inputs(
            recipe,
            label,
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
    }

    pub fn stage_with_inputs(
        &self,
        recipe: SketchRecipe,
        label: impl Into<String>,
        inputs: &SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let label = checked_label(label)?;
        let evaluation = evaluate_recipe(self, &recipe, inputs, precision)?;
        let consumed = recipe.consumed_entities();
        let mut candidate = self.clone();
        let operation_id = candidate.allocate_operation()?;
        let previous_points = candidate.high_water_marks().point();
        let previous_entities = candidate.high_water_marks().entity();
        let operation = candidate.instantiate_evaluation(
            operation_id,
            recipe,
            &evaluation.points,
            &evaluation.curves,
        )?;
        candidate.push_operation(operation);
        let mut impact = SketchImpactReport {
            profile_changed: evaluation
                .curves
                .iter()
                .any(|curve| curve.entity_role == crate::SketchEntityRole::Profile),
            construction_changed: evaluation
                .curves
                .iter()
                .any(|curve| curve.entity_role == crate::SketchEntityRole::Construction),
            ..SketchImpactReport::default()
        };
        supersede_consumed_entities(&mut candidate, operation_id, &consumed, &mut impact)?;
        candidate.set_revision(next_revision(self.revision())?);
        candidate.validate_with_inputs(inputs, precision)?;
        impact.inserted_operations.insert(operation_id);
        impact.inserted_points.extend(
            candidate
                .points()
                .keys()
                .copied()
                .filter(|id| id.get() > previous_points),
        );
        impact.inserted_entities.extend(
            candidate
                .entities()
                .keys()
                .copied()
                .filter(|id| id.get() > previous_entities),
        );
        Ok(SketchTransaction {
            expected_revision: self.revision(),
            label,
            candidate,
            impact,
            inputs: inputs.clone(),
            precision,
        })
    }

    /// Stages one geometric relation as an atomic transaction (ADR 0026, F1).
    ///
    /// A relation adds no geometry: it adds an equation, and the solver moves
    /// existing points to satisfy it. That makes the candidate the natural
    /// home for it — `add_constraint` already refuses and rolls back a
    /// non-converged system, so a conflicting relation leaves this definition,
    /// its identifiers, and its revision untouched, exactly as a rejected
    /// recipe does. Confirmation goes through the same tick/Enter gate as
    /// every other model change.
    pub fn stage_constraint(
        &self,
        kind: SketchConstraintKind,
        label: impl Into<String>,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        self.stage_constraints(vec![kind], label, precision)
    }

    /// Stages several relations as one atomic edit.
    ///
    /// Pinning a line is two `Fixed` equations, one per endpoint, and the user
    /// asked for one relation — so they arrive, and are refused, together.
    pub fn stage_constraints(
        &self,
        kinds: Vec<SketchConstraintKind>,
        label: impl Into<String>,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let label = checked_label(label)?;
        if kinds.is_empty() {
            return Err(SketchTransactionError::NoChange);
        }
        let before = self
            .solve_constraints(precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;
        let mut candidate = self.clone();
        for kind in kinds {
            candidate
                .add_constraint(kind, precision)
                .map_err(SketchTransactionError::ConstraintRejected)?;
        }
        let after = candidate
            .solve_constraints(precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;

        // The impact is whichever points the solver actually moved. A relation
        // the sketch already satisfied moves nothing, and is still worth
        // recording: it is what makes the sketch stay that way.
        let mut impact = SketchImpactReport {
            profile_changed: true,
            ..SketchImpactReport::default()
        };
        for (point, position) in &after.positions {
            let moved = before
                .positions
                .get(point)
                .is_none_or(|previous| !positions_agree(*previous, *position, precision));
            if moved {
                impact.changed_points.insert(*point);
                for (entity, record) in candidate.entities() {
                    if record.active && record.geometry.referenced_points().contains(point) {
                        impact.changed_entities.insert(*entity);
                    }
                }
            }
        }
        // Each `add_constraint` advances the revision; the batch publishes one
        // successor however many equations it carries.
        candidate.set_revision(next_revision(self.revision())?);
        candidate.validate_with_inputs(&SketchInputValues::default(), precision)?;
        Ok(SketchTransaction {
            expected_revision: self.revision(),
            label,
            candidate,
            impact,
            inputs: SketchInputValues::default(),
            precision,
        })
    }

    /// Stages a branch-replacing edit. This is the explicit API used by
    /// modifier controllers; it rejects creation-only recipes while retaining
    /// the same atomic preview/confirm implementation as `stage`.
    pub fn stage_modifier(
        &self,
        recipe: SketchRecipe,
        label: impl Into<String>,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        self.stage_modifier_with_inputs(
            recipe,
            label,
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
    }

    pub fn stage_modifier_with_inputs(
        &self,
        recipe: SketchRecipe,
        label: impl Into<String>,
        inputs: &SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        if !recipe.is_modifier() {
            return Err(SketchTransactionError::NotAModifier);
        }
        self.stage_with_inputs(recipe, label, inputs, precision)
    }

    /// Stages a Trim as one persistent modifier transaction. The exact span is
    /// recomputed from the target, role-compatible limits, and model-space pick
    /// on every replay. Confirmation atomically publishes retained fragments
    /// and supersedes the source; cancellation leaves IDs and revision neutral.
    pub fn stage_trim(
        &self,
        target: SketchEntityId,
        mut limits: Vec<SketchEntityId>,
        pick: SketchPoint2,
        label: impl Into<String>,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        limits.sort_unstable();
        limits.dedup();
        self.stage_modifier_with_inputs(
            SketchRecipe::Trim {
                target,
                limits,
                pick,
            },
            label,
            &SketchInputValues::default(),
            precision,
        )
    }

    /// Replaces one operation recipe and deterministically replays it and all
    /// later operations. Matching semantic output roles retain their IDs.
    pub fn stage_replace(
        &self,
        operation: SketchOperationId,
        recipe: SketchRecipe,
        label: impl Into<String>,
        inputs: &SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let label = checked_label(label)?;
        let start_index = self
            .operations
            .iter()
            .position(|record| record.id == operation && record.active)
            .ok_or(SketchTransactionError::MissingActiveOperation(operation))?;
        let mut candidate = self.clone();
        let mut impact = SketchImpactReport::default();
        restore_entities_superseded_by(&mut candidate, operation, &mut impact);
        candidate.operations[start_index].recipe = recipe;
        for index in start_index..candidate.operations.len() {
            if !candidate.operations[index].active {
                continue;
            }
            let operation_id = candidate.operations[index].id;
            let operation_recipe = candidate.operations[index].recipe.clone();
            restore_entities_superseded_by(&mut candidate, operation_id, &mut impact);
            deactivate_operation_outputs(&mut candidate, index);
            let evaluation = evaluate_recipe(&candidate, &operation_recipe, inputs, precision)?;
            apply_replacement(&mut candidate, index, evaluation, &mut impact)?;
            let consumed = operation_recipe.consumed_entities();
            supersede_consumed_entities(&mut candidate, operation_id, &consumed, &mut impact)?;
            impact.changed_operations.insert(operation_id);
        }
        candidate.set_revision(next_revision(self.revision())?);
        candidate.validate_with_inputs(inputs, precision)?;
        Ok(SketchTransaction {
            expected_revision: self.revision(),
            label,
            candidate,
            impact,
            inputs: inputs.clone(),
            precision,
        })
    }

    /// Replaces one operation's recipe and drags whatever is joined to it along
    /// behind, so a relation survives the edit instead of being broken by it.
    ///
    /// The plain [`Self::stage_replace`] moves only the operation it is given.
    /// Anything coincident with a point that operation authored then sits in
    /// the wrong place, and the read-time solve splits the difference between
    /// the two, which is why dragging one end of a joined pair used to move it
    /// half as far as the pointer went and drag its partner half way to meet
    /// it. Here the edit is solved with its own points anchored — it is the
    /// authority, the relations are not — and every point the solver has to
    /// move in consequence is written back into its own operation's recipe.
    /// The sketch that results satisfies its relations outright, so the next
    /// ordinary solve has nothing left to average.
    ///
    /// Two limits are worth naming. A follower point that its recipe cannot
    /// state as a literal (see [`SketchRecipe::set_authored_point`]) is left
    /// where it is, and the ordinary solve goes on splitting the difference
    /// for it. And an edit whose own points cannot all be held at once —
    /// dragging both ends of a line that carries a distance relation, say —
    /// falls back to the plain replacement rather than refusing the drag.
    pub fn stage_replace_pulling_followers(
        &self,
        operation: SketchOperationId,
        recipe: SketchRecipe,
        label: impl Into<String>,
        inputs: &SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let transaction = self.stage_replace(operation, recipe, label, inputs, precision)?;
        let anchored = points_authored_by(&transaction.candidate, operation);
        Ok(pull_followers(transaction, anchored, precision))
    }

    /// Restates what one relation measures and drags whatever is joined to it
    /// along behind, exactly as [`Self::stage_replace_pulling_followers`] does
    /// for a recipe.
    ///
    /// A dimension the user types into is an edit, and an edit outranks the
    /// solver's freedom to share the movement out (ADR 0035). `held` is the end
    /// the dimension is measured *from*: it stays, and the other end is what
    /// travels. Holding neither end is legitimate — the solver then moves both,
    /// symmetrically, which is the fair answer when nothing distinguishes them.
    ///
    /// A value the relation system cannot satisfy is refused, and the sketch is
    /// left exactly as it was. That is the same bargain every other sketch edit
    /// makes: a dimension that cannot hold is not quietly rounded to one that
    /// can.
    pub fn stage_relation_measurement(
        &self,
        constraint: SketchConstraintId,
        measurement: f64,
        held: Option<SketchPointId>,
        label: impl Into<String>,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let label = checked_label(label)?;
        let Some(record) = self.constraints().get(&constraint) else {
            return Err(SketchTransactionError::ConstraintRejected(
                ConstraintError::MissingConstraint(constraint),
            ));
        };
        let restated = record
            .kind
            .with_measurement(measurement)
            .ok_or(SketchTransactionError::RelationHasNoMeasurement(constraint))?;
        if restated == record.kind {
            return Err(SketchTransactionError::NoChange);
        }
        let before = self
            .solve_constraints(precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;
        let datum = restated.datum_points();
        let mut candidate = self.clone();
        candidate
            .set_constraint_kind(constraint, restated, precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;
        // Whatever the caller named, plus whatever the relation is measured
        // from. A dimension typed against a datum must move the thing being
        // located and leave the datum where it is.
        let anchored = held.into_iter().chain(datum).collect::<BTreeSet<_>>();
        let after = candidate
            .solve_constraints_anchoring(&anchored, precision)
            .map_err(SketchTransactionError::ConstraintRejected)?;

        let mut impact = SketchImpactReport {
            profile_changed: true,
            ..SketchImpactReport::default()
        };
        for (point, position) in &after.positions {
            let moved = before
                .positions
                .get(point)
                .is_none_or(|previous| !positions_agree(*previous, *position, precision));
            if moved {
                impact.changed_points.insert(*point);
                for (entity, record) in candidate.entities() {
                    if record.active && record.geometry.referenced_points().contains(point) {
                        impact.changed_entities.insert(*entity);
                    }
                }
            }
        }
        // `set_constraint_kind` advances the revision itself; the transaction
        // publishes one successor of the revision it was staged against.
        candidate.set_revision(next_revision(self.revision())?);
        candidate.validate_with_inputs(&SketchInputValues::default(), precision)?;
        let transaction = SketchTransaction {
            expected_revision: self.revision(),
            label,
            candidate,
            impact,
            inputs: SketchInputValues::default(),
            precision,
        };
        Ok(pull_followers(transaction, anchored, precision))
    }

    pub fn stage_retire_operation(
        &self,
        operation: SketchOperationId,
        policy: RetirementPolicy,
        label: impl Into<String>,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        self.stage_retire_operation_with_inputs(
            operation,
            policy,
            label,
            &SketchInputValues::default(),
            precision,
        )
    }

    pub fn stage_retire_operation_with_inputs(
        &self,
        operation: SketchOperationId,
        policy: RetirementPolicy,
        label: impl Into<String>,
        inputs: &SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let label = checked_label(label)?;
        let target = self
            .operation(operation)
            .filter(|record| record.active)
            .ok_or(SketchTransactionError::MissingActiveOperation(operation))?;
        let mut retire = BTreeSet::from([target.id]);
        let mut changed = true;
        while changed {
            changed = false;
            let retired_points = self
                .operations
                .iter()
                .filter(|record| retire.contains(&record.id))
                .flat_map(|record| record.outputs.values())
                .filter_map(|output| match output {
                    SketchOutputRef::Point(id) => Some(*id),
                    SketchOutputRef::Curve(_) => None,
                })
                .collect::<BTreeSet<_>>();
            let retired_entities = self
                .operations
                .iter()
                .filter(|record| retire.contains(&record.id))
                .flat_map(|record| record.outputs.values())
                .filter_map(|output| match output {
                    SketchOutputRef::Curve(id) => Some(*id),
                    SketchOutputRef::Point(_) => None,
                })
                .collect::<BTreeSet<_>>();
            for record in self.active_operations() {
                if retire.contains(&record.id) {
                    continue;
                }
                if record
                    .recipe
                    .referenced_points()
                    .iter()
                    .any(|point| retired_points.contains(point))
                    || record
                        .recipe
                        .referenced_entities()
                        .iter()
                        .any(|entity| retired_entities.contains(entity))
                {
                    if policy == RetirementPolicy::RejectDependents {
                        return Err(SketchTransactionError::DependentOperations {
                            operation,
                            dependents: vec![record.id],
                        });
                    }
                    changed |= retire.insert(record.id);
                }
            }
        }

        let mut candidate = self.clone();
        let mut impact = SketchImpactReport::default();
        for operation_id in retire {
            retire_operation(&mut candidate, operation_id, &mut impact)?;
        }
        candidate.set_revision(next_revision(self.revision())?);
        candidate.validate_with_inputs(inputs, precision)?;
        Ok(SketchTransaction {
            expected_revision: self.revision(),
            label,
            candidate,
            impact,
            inputs: inputs.clone(),
            precision,
        })
    }

    /// Stages visibility as a normal revisioned edit so it uses the same
    /// green-tick/red-cross and undo contract as geometry edits.
    pub fn stage_entity_visibility(
        &self,
        entity: SketchEntityId,
        visible: bool,
        label: impl Into<String>,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        self.stage_entity_visibility_with_inputs(
            entity,
            visible,
            label,
            &SketchInputValues::default(),
            precision,
        )
    }

    pub fn stage_entity_visibility_with_inputs(
        &self,
        entity: SketchEntityId,
        visible: bool,
        label: impl Into<String>,
        inputs: &SketchInputValues,
        precision: PrecisionPolicy,
    ) -> Result<SketchTransaction, SketchTransactionError> {
        let label = checked_label(label)?;
        let mut candidate = self.clone();
        let record = candidate
            .entity_mut(entity)
            .filter(|record| record.active)
            .ok_or(SketchTransactionError::MissingActiveEntity(entity))?;
        if record.visible == visible {
            return Err(SketchTransactionError::NoChange);
        }
        record.visible = visible;
        candidate.set_revision(next_revision(self.revision())?);
        candidate.validate_with_inputs(inputs, precision)?;
        let mut impact = SketchImpactReport {
            visibility_changed: true,
            ..SketchImpactReport::default()
        };
        impact.changed_entities.insert(entity);
        Ok(SketchTransaction {
            expected_revision: self.revision(),
            label,
            candidate,
            impact,
            inputs: inputs.clone(),
            precision,
        })
    }

    /// Revalidates and atomically publishes a staged candidate.
    pub fn commit(
        &mut self,
        transaction: SketchTransaction,
        confirmation: ConfirmationSource,
    ) -> Result<SketchCommit, SketchTransactionError> {
        let precision = transaction.precision;
        self.commit_with_precision(transaction, confirmation, precision)
    }

    pub fn commit_with_precision(
        &mut self,
        transaction: SketchTransaction,
        confirmation: ConfirmationSource,
        precision: PrecisionPolicy,
    ) -> Result<SketchCommit, SketchTransactionError> {
        if self.revision() != transaction.expected_revision {
            return Err(SketchTransactionError::StaleRevision {
                expected: transaction.expected_revision,
                actual: self.revision(),
            });
        }
        transaction
            .candidate
            .validate_with_inputs(&transaction.inputs, precision)?;
        let commit = SketchCommit {
            revision: transaction.candidate.revision(),
            confirmation,
            label: transaction.label,
            impact: transaction.impact,
        };
        *self = transaction.candidate;
        Ok(commit)
    }
}

fn deactivate_operation_outputs(candidate: &mut SketchDefinition, operation_index: usize) {
    let outputs = candidate.operations[operation_index].outputs.clone();
    for output in outputs.values() {
        match output {
            SketchOutputRef::Point(id) => {
                if let Some(point) = candidate.point_mut(*id) {
                    point.active = false;
                }
            }
            SketchOutputRef::Curve(id) => {
                if let Some(entity) = candidate.entity_mut(*id) {
                    entity.active = false;
                }
            }
        }
    }
}

fn apply_replacement(
    candidate: &mut SketchDefinition,
    operation_index: usize,
    evaluation: PrimitiveEvaluation,
    impact: &mut SketchImpactReport,
) -> Result<(), SketchTransactionError> {
    let operation_id = candidate.operations[operation_index].id;
    let old_outputs = candidate.operations[operation_index].outputs.clone();
    for output in old_outputs.values() {
        match output {
            SketchOutputRef::Point(id) => {
                if let Some(point) = candidate.point_mut(*id) {
                    point.active = false;
                }
            }
            SketchOutputRef::Curve(id) => {
                if let Some(entity) = candidate.entity_mut(*id) {
                    entity.active = false;
                }
            }
        }
    }

    let mut point_ids = BTreeMap::new();
    let mut outputs = BTreeMap::new();
    for point in evaluation.points {
        let output_role = OutputRole::Point(point.role);
        let id = match old_outputs.get(&output_role) {
            Some(SketchOutputRef::Point(id)) => {
                let record = candidate
                    .point_mut(*id)
                    .ok_or(SketchValidationError::MissingPoint { point: *id })?;
                record.owner = SketchOutputOwner {
                    operation: operation_id,
                    role: point.role,
                };
                record.evaluated_position = point.position;
                record.active = true;
                impact.changed_points.insert(*id);
                *id
            }
            _ => {
                let id = candidate.allocate_point()?;
                candidate.insert_point(SketchPointRecord {
                    id,
                    owner: SketchOutputOwner {
                        operation: operation_id,
                        role: point.role,
                    },
                    evaluated_position: point.position,
                    active: true,
                });
                impact.inserted_points.insert(id);
                id
            }
        };
        point_ids.insert(point.role, id);
        outputs.insert(output_role, SketchOutputRef::Point(id));
    }

    for curve in evaluation.curves {
        let output_role = OutputRole::Curve(curve.role);
        let geometry = instantiate_curve(curve.geometry, &point_ids)?;
        let id = match old_outputs.get(&output_role) {
            Some(SketchOutputRef::Curve(id)) => {
                let record = candidate
                    .entity_mut(*id)
                    .ok_or(SketchTransactionError::MissingActiveEntity(*id))?;
                record.role = curve.entity_role;
                record.geometry = geometry;
                record.provenance = CurveProvenance {
                    operation: operation_id,
                    role: curve.role,
                };
                record.active = true;
                record.superseded_by = None;
                impact.changed_entities.insert(*id);
                *id
            }
            _ => {
                let id = candidate.allocate_entity()?;
                candidate.insert_entity(SketchEntityRecord {
                    id,
                    role: curve.entity_role,
                    geometry,
                    provenance: CurveProvenance {
                        operation: operation_id,
                        role: curve.role,
                    },
                    visible: true,
                    active: true,
                    superseded_by: None,
                });
                impact.inserted_entities.insert(id);
                id
            }
        };
        outputs.insert(output_role, SketchOutputRef::Curve(id));
        impact.profile_changed |= curve.entity_role == crate::SketchEntityRole::Profile;
        impact.construction_changed |= curve.entity_role == crate::SketchEntityRole::Construction;
    }

    for (role, output) in old_outputs {
        if outputs.contains_key(&role) {
            continue;
        }
        match output {
            SketchOutputRef::Point(id) => {
                impact.retired_points.insert(id);
            }
            SketchOutputRef::Curve(id) => {
                impact.retired_entities.insert(id);
            }
        }
    }
    candidate.operations[operation_index].outputs = outputs;
    Ok(())
}

fn supersede_consumed_entities(
    candidate: &mut SketchDefinition,
    modifier: SketchOperationId,
    sources: &[SketchEntityId],
    impact: &mut SketchImpactReport,
) -> Result<(), SketchTransactionError> {
    for source in sources {
        let record = candidate
            .entity_mut(*source)
            .filter(|record| record.active && record.superseded_by.is_none())
            .ok_or(SketchTransactionError::MissingActiveEntity(*source))?;
        record.active = false;
        record.superseded_by = Some(modifier);
        impact.changed_entities.insert(*source);
        impact.retired_entities.insert(*source);
        impact.superseded_entities.insert(*source);
        impact.profile_changed |= record.role == crate::SketchEntityRole::Profile;
        impact.construction_changed |= record.role == crate::SketchEntityRole::Construction;
    }
    Ok(())
}

fn restore_entities_superseded_by(
    candidate: &mut SketchDefinition,
    modifier: SketchOperationId,
    impact: &mut SketchImpactReport,
) {
    let sources = candidate
        .entities()
        .values()
        .filter(|entity| entity.superseded_by == Some(modifier))
        .map(|entity| (entity.id, entity.provenance.operation, entity.role))
        .collect::<Vec<_>>();
    for (entity, owner, role) in sources {
        let owner_active = candidate
            .operation(owner)
            .is_some_and(|operation| operation.active);
        if let Some(record) = candidate.entity_mut(entity) {
            record.superseded_by = None;
            record.active = owner_active;
        }
        impact.changed_entities.insert(entity);
        if owner_active {
            impact.restored_entities.insert(entity);
            impact.retired_entities.remove(&entity);
        }
        impact.profile_changed |= role == crate::SketchEntityRole::Profile;
        impact.construction_changed |= role == crate::SketchEntityRole::Construction;
    }
}

fn retire_operation(
    candidate: &mut SketchDefinition,
    operation: SketchOperationId,
    impact: &mut SketchImpactReport,
) -> Result<(), SketchTransactionError> {
    restore_entities_superseded_by(candidate, operation, impact);
    let index = candidate
        .operations
        .iter()
        .position(|record| record.id == operation && record.active)
        .ok_or(SketchTransactionError::MissingActiveOperation(operation))?;
    let outputs = candidate.operations[index].outputs.clone();
    candidate.operations[index].active = false;
    impact.retired_operations.insert(operation);
    for output in outputs.values() {
        match output {
            SketchOutputRef::Point(id) => {
                if let Some(point) = candidate.point_mut(*id) {
                    point.active = false;
                }
                impact.retired_points.insert(*id);
            }
            SketchOutputRef::Curve(id) => {
                if let Some(entity) = candidate.entity_mut(*id) {
                    impact.profile_changed |= entity.role == crate::SketchEntityRole::Profile;
                    impact.construction_changed |=
                        entity.role == crate::SketchEntityRole::Construction;
                    entity.active = false;
                }
                impact.retired_entities.insert(*id);
            }
        }
    }
    Ok(())
}

fn checked_label(label: impl Into<String>) -> Result<String, SketchTransactionError> {
    let label = label.into();
    if label.trim().is_empty() {
        Err(SketchTransactionError::EmptyLabel)
    } else {
        Ok(label)
    }
}

fn next_revision(revision: SketchRevision) -> Result<SketchRevision, SketchTransactionError> {
    revision
        .checked_next()
        .ok_or(SketchTransactionError::RevisionExhausted)
}

#[derive(Clone, Debug, PartialEq)]
pub enum SketchTransactionError {
    Validation(SketchValidationError),
    EmptyLabel,
    NoChange,
    RevisionExhausted,
    MissingActiveOperation(SketchOperationId),
    MissingActiveEntity(SketchEntityId),
    NotAModifier,
    /// The relation named holds no number, so there is nothing to retype: a
    /// perpendicular is perpendicular, and typing into it would have to make it
    /// a different relation.
    RelationHasNoMeasurement(SketchConstraintId),
    /// The solver refused the relation: the system is conflicting, or it names
    /// a point that is missing, inactive, or repeated.
    ConstraintRejected(crate::ConstraintError),
    DependentOperations {
        operation: SketchOperationId,
        dependents: Vec<SketchOperationId>,
    },
    StaleRevision {
        expected: SketchRevision,
        actual: SketchRevision,
    },
}

impl fmt::Display for SketchTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::EmptyLabel => formatter.write_str("sketch transaction label cannot be empty"),
            Self::NoChange => {
                formatter.write_str("sketch transaction does not change the definition")
            }
            Self::RevisionExhausted => formatter.write_str("sketch revision space is exhausted"),
            Self::MissingActiveOperation(operation) => {
                write!(formatter, "operation {operation} is missing or retired")
            }
            Self::MissingActiveEntity(entity) => {
                write!(formatter, "entity {entity} is missing or retired")
            }
            Self::NotAModifier => formatter.write_str("a modifier recipe is required"),
            Self::RelationHasNoMeasurement(constraint) => {
                write!(
                    formatter,
                    "relation {constraint} does not hold a measurement"
                )
            }
            Self::ConstraintRejected(error) => write!(formatter, "relation refused: {error}"),
            Self::DependentOperations {
                operation,
                dependents,
            } => write!(
                formatter,
                "operation {operation} is still referenced by dependent operations {dependents:?}"
            ),
            Self::StaleRevision { expected, actual } => write!(
                formatter,
                "staged sketch revision {expected} is stale; current revision is {actual}"
            ),
        }
    }
}

impl std::error::Error for SketchTransactionError {}

impl From<SketchValidationError> for SketchTransactionError {
    fn from(error: SketchValidationError) -> Self {
        Self::Validation(error)
    }
}

/// Bounded local sketch undo/redo journal. Candidates are never journalled;
/// callers record only the pre-commit definition after a successful confirm.
#[derive(Clone, Debug)]
pub struct SketchUndoJournal {
    capacity: usize,
    undo: VecDeque<SketchDefinition>,
    redo: VecDeque<SketchDefinition>,
}

impl SketchUndoJournal {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
        }
    }

    pub fn confirm(
        &mut self,
        definition: &mut SketchDefinition,
        transaction: SketchTransaction,
        source: ConfirmationSource,
        precision: PrecisionPolicy,
    ) -> Result<SketchCommit, SketchTransactionError> {
        let before = definition.clone();
        let commit = definition.commit_with_precision(transaction, source, precision)?;
        self.push_undo(before);
        self.redo.clear();
        Ok(commit)
    }

    pub fn undo(&mut self, definition: &mut SketchDefinition) -> bool {
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        let published_high_water = definition.high_water_marks();
        self.redo.push_back(std::mem::replace(definition, previous));
        definition.preserve_high_water_marks(published_high_water);
        true
    }

    pub fn redo(&mut self, definition: &mut SketchDefinition) -> bool {
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        let published_high_water = definition.high_water_marks();
        self.push_undo(std::mem::replace(definition, next));
        definition.preserve_high_water_marks(published_high_water);
        true
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn push_undo(&mut self, definition: SketchDefinition) {
        if self.undo.len() == self.capacity {
            self.undo.pop_front();
        }
        self.undo.push_back(definition);
    }
}

impl Default for SketchUndoJournal {
    fn default() -> Self {
        Self::new(128)
    }
}

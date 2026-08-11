use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ConfirmationSource, CurveOutputRole, Length, OutputRole, PointInput, PointOutputRole,
    RetirementPolicy, SignedLength, SketchDefinition, SketchInputId, SketchInputKey,
    SketchInputValues, SketchOutputRef, SketchPoint2, SketchRecipe, SketchRevision,
    SketchTransactionError, SketchUndoJournal, SketchValue,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn line(start: (f64, f64), end: (f64, f64)) -> SketchRecipe {
    SketchRecipe::Line {
        start: point(start.0, start.1),
        end: point(end.0, end.1),
    }
}

fn signed(value: f64) -> SketchValue<SignedLength> {
    SketchValue::Literal(SignedLength::new(value).expect("finite signed length"))
}

fn length(value: f64) -> SketchValue<Length> {
    SketchValue::Literal(Length::new(value).expect("positive length"))
}

#[test]
fn green_tick_commits_once_and_red_cross_is_bitwise_neutral() {
    let mut sketch = SketchDefinition::new();
    let original = sketch.clone();
    let cancelled = sketch
        .stage(line((0.0, 0.0), (10.0, 0.0)), "Line")
        .expect("stage")
        .cancel();
    assert_eq!(cancelled.unchanged_revision, SketchRevision::INITIAL);
    assert_eq!(sketch, original);

    let transaction = sketch
        .stage(line((0.0, 0.0), (10.0, 0.0)), "Line")
        .expect("stage");
    assert_eq!(sketch.revision(), SketchRevision::INITIAL);
    assert_eq!(transaction.preview().active_entities().count(), 1);
    let commit = sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    assert_eq!(commit.revision, SketchRevision::new(1));
    assert_eq!(sketch.revision(), SketchRevision::new(1));
    assert_eq!(sketch.active_operations().count(), 1);
    assert_eq!(sketch.active_entities().count(), 1);
}

#[test]
fn a_bare_enter_uses_the_same_atomic_commit_path() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch
        .stage(
            SketchRecipe::Polyline {
                vertices: vec![point(0.0, 0.0), point(10.0, 0.0), point(10.0, 5.0)],
                closed: false,
                construction: false,
            },
            "Polyline",
        )
        .expect("stage complete local chain");
    assert_eq!(transaction.preview().active_entities().count(), 2);
    let commit = sketch
        .commit(transaction, ConfirmationSource::BareEnter)
        .expect("commit entire chain");
    assert_eq!(commit.confirmation, ConfirmationSource::BareEnter);
    assert_eq!(sketch.revision(), SketchRevision::new(1));
    assert_eq!(sketch.operations().len(), 1);
}

#[test]
fn stale_candidate_never_overwrites_a_newer_revision() {
    let mut sketch = SketchDefinition::new();
    let first = sketch
        .stage(line((0.0, 0.0), (5.0, 0.0)), "First")
        .expect("stage first");
    let stale = sketch
        .stage(line((0.0, 1.0), (5.0, 1.0)), "Stale")
        .expect("stage stale");
    sketch
        .commit(first, ConfirmationSource::GreenTick)
        .expect("commit first");
    let before = sketch.clone();
    assert_eq!(
        sketch
            .commit(stale, ConfirmationSource::GreenTick)
            .expect_err("stale candidate must fail"),
        SketchTransactionError::StaleRevision {
            expected: SketchRevision::INITIAL,
            actual: SketchRevision::new(1)
        }
    );
    assert_eq!(sketch, before);
}

#[test]
fn replacing_dimensions_retains_semantic_output_ids() {
    let mut sketch = SketchDefinition::new();
    let rectangle = SketchRecipe::TwoPointRectangle {
        first_corner: point(0.0, 0.0),
        width: signed(10.0),
        height: signed(5.0),
    };
    let transaction = sketch.stage(rectangle, "Rectangle").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let operation = sketch.operations()[0].id;
    let original_outputs = sketch.operations()[0].outputs.clone();
    let original_marks = sketch.high_water_marks();

    let replacement = SketchRecipe::TwoPointRectangle {
        first_corner: point(-2.0, -3.0),
        width: signed(25.0),
        height: signed(8.0),
    };
    let transaction = sketch
        .stage_replace(
            operation,
            replacement,
            "Resize rectangle",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("stage replacement");
    assert_eq!(
        transaction.preview().operations()[0].outputs,
        original_outputs
    );
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit replacement");
    assert_eq!(sketch.operations()[0].outputs, original_outputs);
    assert_eq!(sketch.high_water_marks(), original_marks);
    assert_eq!(sketch.revision(), SketchRevision::new(2));
}

#[test]
fn removed_roles_are_tombstones_and_allocators_never_reuse_them() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch
        .stage(line((0.0, 0.0), (4.0, 0.0)), "Line one")
        .expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let operation = sketch.operations()[0].id;
    let old_entity =
        match sketch.operations()[0].outputs[&OutputRole::Curve(CurveOutputRole::Curve)] {
            SketchOutputRef::Curve(id) => id,
            SketchOutputRef::Point(_) => panic!("curve output"),
        };
    let old_point_high_water = sketch.high_water_marks().point();

    let retire = sketch
        .stage_retire_operation(
            operation,
            RetirementPolicy::RejectDependents,
            "Delete line",
            PrecisionPolicy::default(),
        )
        .expect("stage retire");
    sketch
        .commit(retire, ConfirmationSource::GreenTick)
        .expect("retire");
    assert!(
        !sketch
            .entity(old_entity)
            .expect("tombstone retained")
            .active
    );

    let transaction = sketch
        .stage(line((1.0, 1.0), (5.0, 1.0)), "Line two")
        .expect("stage second");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit second");
    let new_entity = sketch
        .active_entities()
        .next()
        .expect("active replacement line")
        .id;
    assert!(new_entity.get() > old_entity.get());
    assert!(sketch.high_water_marks().point() > old_point_high_water);
}

#[test]
fn point_reuse_is_acyclic_and_retirement_can_reject_or_cascade_dependents() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch
        .stage(line((0.0, 0.0), (5.0, 0.0)), "Parent")
        .expect("stage parent");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit parent");
    let parent = sketch.operations()[0].id;
    let shared = match sketch.operations()[0].outputs[&OutputRole::Point(PointOutputRole::End)] {
        SketchOutputRef::Point(id) => id,
        SketchOutputRef::Curve(_) => panic!("point output"),
    };
    let child_recipe = SketchRecipe::Line {
        start: PointInput::Existing(shared),
        end: point(5.0, 5.0),
    };
    let transaction = sketch.stage(child_recipe, "Child").expect("stage child");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit child");
    assert!(sketch.validate(PrecisionPolicy::default()).is_ok());

    assert!(matches!(
        sketch.stage_retire_operation(
            parent,
            RetirementPolicy::RejectDependents,
            "Reject",
            PrecisionPolicy::default()
        ),
        Err(SketchTransactionError::DependentOperations { .. })
    ));
    let cascade = sketch
        .stage_retire_operation(
            parent,
            RetirementPolicy::CascadeDependents,
            "Cascade",
            PrecisionPolicy::default(),
        )
        .expect("stage cascade");
    assert_eq!(cascade.impact().retired_operations.len(), 2);
    sketch
        .commit(cascade, ConfirmationSource::GreenTick)
        .expect("commit cascade");
    assert_eq!(sketch.active_operations().count(), 0);
    assert_eq!(sketch.active_entities().count(), 0);
}

#[test]
fn local_undo_and_redo_record_only_successful_confirmations() {
    let mut sketch = SketchDefinition::new();
    let mut journal = SketchUndoJournal::new(8);
    let transaction = sketch
        .stage(
            SketchRecipe::CentrePointCircle {
                center: point(0.0, 0.0),
                radius: length(5.0),
                radial_angle: SketchValue::Literal(
                    artificer_sketch::Angle::radians(0.0).expect("angle"),
                ),
            },
            "Circle",
        )
        .expect("stage");
    journal
        .confirm(
            &mut sketch,
            transaction,
            ConfirmationSource::GreenTick,
            PrecisionPolicy::default(),
        )
        .expect("commit");
    let committed = sketch.clone();
    assert!(journal.undo(&mut sketch));
    assert_eq!(sketch.active_operations().count(), 0);
    assert_eq!(sketch.active_entities().count(), 0);
    assert_eq!(sketch.revision(), SketchRevision::INITIAL);
    assert_eq!(sketch.high_water_marks(), committed.high_water_marks());
    assert!(journal.redo(&mut sketch));
    assert_eq!(sketch, committed);
}

#[test]
fn allocating_after_local_undo_never_reuses_a_published_identity() {
    let mut sketch = SketchDefinition::new();
    let mut journal = SketchUndoJournal::new(8);
    let first = sketch
        .stage(line((0.0, 0.0), (4.0, 0.0)), "First line")
        .expect("stage first line");
    journal
        .confirm(
            &mut sketch,
            first,
            ConfirmationSource::GreenTick,
            PrecisionPolicy::default(),
        )
        .expect("commit first line");
    let published = sketch.high_water_marks();

    assert!(journal.undo(&mut sketch));
    assert_eq!(sketch.high_water_marks(), published);

    let replacement = sketch
        .stage(line((1.0, 1.0), (5.0, 1.0)), "Replacement line")
        .expect("stage replacement line after undo");
    journal
        .confirm(
            &mut sketch,
            replacement,
            ConfirmationSource::BareEnter,
            PrecisionPolicy::default(),
        )
        .expect("commit replacement line");
    let replacement_operation = sketch
        .active_operations()
        .next()
        .expect("replacement operation");
    let replacement_entity = sketch.active_entities().next().expect("replacement entity");
    assert!(replacement_operation.id.get() > published.operation());
    assert!(replacement_entity.id.get() > published.entity());
    assert!(sketch.high_water_marks().point() > published.point());
}

#[test]
fn replacement_budget_counts_the_replacement_not_both_cache_generations() {
    let vertices = (0..1_024)
        .map(|index| {
            let angle = f64::from(index) * std::f64::consts::TAU / 1_024.0;
            point(angle.cos() * 100.0, angle.sin() * 100.0)
        })
        .collect::<Vec<_>>();
    let recipe = SketchRecipe::Polyline {
        vertices,
        closed: true,
        construction: false,
    };
    let mut sketch = SketchDefinition::new();
    let transaction = sketch
        .stage(recipe.clone(), "Dense polyline")
        .expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    assert_eq!(sketch.active_entities().count(), 1_024);
    let operation = sketch.operations()[0].id;
    let replacement = sketch.stage_replace(
        operation,
        recipe,
        "Replay dense polyline",
        &SketchInputValues::default(),
        PrecisionPolicy::default(),
    );
    assert!(
        replacement.is_ok(),
        "replacement must not double-count old caches"
    );
}

#[test]
fn bound_typed_inputs_are_retained_for_commit_time_revalidation() {
    let key = SketchInputKey::new(1).expect("non-zero");
    let width_id = SketchInputId::<SignedLength>::new(key);
    let mut inputs = SketchInputValues::default();
    inputs.insert_signed_length(width_id, SignedLength::new(8.0).expect("width"));
    let recipe = SketchRecipe::TwoPointRectangle {
        first_corner: point(0.0, 0.0),
        width: SketchValue::Input(width_id),
        height: signed(4.0),
    };
    let mut sketch = SketchDefinition::new();
    let transaction = sketch
        .stage_with_inputs(
            recipe,
            "Bound rectangle",
            &inputs,
            PrecisionPolicy::default(),
        )
        .expect("stage bound recipe");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit reruns with staged typed values");
    sketch
        .validate_with_inputs(&inputs, PrecisionPolicy::default())
        .expect("bound graph replays");
}

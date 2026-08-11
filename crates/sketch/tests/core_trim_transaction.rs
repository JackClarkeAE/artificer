use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ConfirmationSource, CurveOutputRole, EvaluatedCurve2, OutputRole, PointInput, SketchDefinition,
    SketchEntityId, SketchOutputRef, SketchPoint2, SketchRecipe, SketchRevision, SketchTransaction,
    SketchTransactionError, SketchValidationError,
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

fn add_line(sketch: &mut SketchDefinition, start: (f64, f64), end: (f64, f64)) -> SketchEntityId {
    let transaction = sketch.stage(line(start, end), "line").expect("stage line");
    let id = *transaction
        .impact()
        .inserted_entities
        .first()
        .expect("one curve");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit line");
    id
}

fn crossing_fixture() -> (SketchDefinition, SketchEntityId, [SketchEntityId; 3]) {
    let mut sketch = SketchDefinition::new();
    let target = add_line(&mut sketch, (-4.0, 0.0), (4.0, 0.0));
    let limits = [
        add_line(&mut sketch, (-2.0, -2.0), (-2.0, 2.0)),
        add_line(&mut sketch, (0.0, -2.0), (0.0, 2.0)),
        add_line(&mut sketch, (2.0, -2.0), (2.0, 2.0)),
    ];
    (sketch, target, limits)
}

fn stage_two_trims(
    sketch: &SketchDefinition,
    target: SketchEntityId,
    limits: [SketchEntityId; 3],
) -> (SketchTransaction, SketchEntityId) {
    let mut transaction = sketch
        .stage_trim(
            target,
            vec![limits[1], limits[0]],
            SketchPoint2::new(-1.0, 0.0),
            "Trim spans",
            PrecisionPolicy::default(),
        )
        .expect("stage first crossing span");
    let first_trim = transaction
        .preview()
        .operations()
        .last()
        .expect("first trim operation");
    let intermediate =
        match first_trim.outputs[&OutputRole::Curve(CurveOutputRole::TrimFragment(1))] {
            SketchOutputRef::Curve(entity) => entity,
            SketchOutputRef::Point(_) => panic!("trim fragment is a curve"),
        };
    transaction
        .append_trim(intermediate, vec![limits[2]], SketchPoint2::new(3.0, 0.0))
        .expect("append second crossing span");
    (transaction, intermediate)
}

fn active_horizontal_ranges(sketch: &SketchDefinition) -> Vec<(f64, f64)> {
    let mut ranges = sketch
        .active_entities()
        .filter_map(|entity| {
            let EvaluatedCurve2::Line { start, end } =
                sketch.evaluated_curve(entity.id).expect("active curve")
            else {
                return None;
            };
            if start.v != 0.0 || end.v != 0.0 {
                return None;
            }
            Some((start.u.min(end.u), start.u.max(end.u)))
        })
        .collect::<Vec<_>>();
    ranges.sort_by(|left, right| left.0.total_cmp(&right.0));
    ranges
}

#[test]
fn trim_middle_span_is_one_atomic_supersession_transaction() {
    let mut sketch = SketchDefinition::new();
    let target = add_line(&mut sketch, (-4.0, 0.0), (4.0, 0.0));
    let first_limit = add_line(&mut sketch, (-1.0, -2.0), (-1.0, 2.0));
    let second_limit = add_line(&mut sketch, (1.0, -2.0), (1.0, 2.0));
    let before = sketch.clone();

    let cancelled = sketch
        .stage_trim(
            target,
            vec![second_limit, first_limit],
            SketchPoint2::new(0.0, 0.0),
            "trim",
            PrecisionPolicy::default(),
        )
        .expect("exact middle span")
        .cancel();
    assert_eq!(cancelled.unchanged_revision, before.revision());
    assert_eq!(sketch, before, "red cross must be bitwise neutral");

    let transaction = sketch
        .stage_trim(
            target,
            vec![first_limit, second_limit],
            SketchPoint2::new(0.0, 0.0),
            "trim",
            PrecisionPolicy::default(),
        )
        .expect("stage exact middle span");
    assert_eq!(transaction.impact().superseded_entities.len(), 1);
    assert!(transaction.impact().superseded_entities.contains(&target));
    assert_eq!(transaction.impact().inserted_entities.len(), 2);
    assert_eq!(
        transaction.preview().active_entities().count(),
        4,
        "two limits plus the two retained target fragments"
    );

    sketch
        .commit(transaction, ConfirmationSource::BareEnter)
        .expect("commit whole trim");
    let source = sketch.entity(target).expect("source tombstone retained");
    assert!(!source.active);
    assert!(source.superseded_by.is_some());
    assert_eq!(sketch.active_entities().count(), 4);
}

#[test]
fn invalid_trim_is_rejected_without_allocating_or_advancing_revision() {
    let mut sketch = SketchDefinition::new();
    let target = add_line(&mut sketch, (0.0, 0.0), (4.0, 0.0));
    let disjoint = add_line(&mut sketch, (8.0, -1.0), (8.0, 1.0));
    let before = sketch.clone();

    assert!(
        sketch
            .stage_trim(
                target,
                vec![disjoint],
                SketchPoint2::new(2.0, 0.0),
                "invalid trim",
                PrecisionPolicy::default(),
            )
            .is_err()
    );
    assert_eq!(sketch, before);
}

#[test]
fn two_trim_spans_commit_behind_one_tick_and_one_revision() {
    let (mut sketch, target, limits) = crossing_fixture();
    let original_revision = sketch.revision();
    let (transaction, intermediate) = stage_two_trims(&sketch, target, limits);

    assert_eq!(transaction.expected_revision(), original_revision);
    assert_eq!(
        transaction.preview().revision(),
        SketchRevision::new(original_revision.get() + 1)
    );
    assert_eq!(transaction.impact().inserted_operations.len(), 2);
    assert_eq!(transaction.impact().inserted_entities.len(), 2);
    assert!(
        !transaction
            .impact()
            .inserted_entities
            .contains(&intermediate),
        "a superseded provisional fragment is not an active insertion"
    );
    assert!(
        !transaction
            .impact()
            .changed_entities
            .contains(&intermediate)
            && !transaction
                .impact()
                .retired_entities
                .contains(&intermediate)
            && !transaction
                .impact()
                .superseded_entities
                .contains(&intermediate),
        "net impact excludes an entity that never existed in the live sketch"
    );
    assert_eq!(
        active_horizontal_ranges(transaction.preview()),
        vec![(-4.0, -2.0), (0.0, 2.0)]
    );

    let commit = sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("one tick commits both trims");
    assert_eq!(
        commit.revision,
        SketchRevision::new(original_revision.get() + 1)
    );
    assert_eq!(sketch.revision(), commit.revision);
    assert_eq!(
        active_horizontal_ranges(&sketch),
        vec![(-4.0, -2.0), (0.0, 2.0)]
    );
}

#[test]
fn cancelling_a_trim_batch_is_revision_and_identity_neutral() {
    let (sketch, target, limits) = crossing_fixture();
    let before = sketch.clone();
    let (transaction, _) = stage_two_trims(&sketch, target, limits);
    assert!(
        transaction.preview().high_water_marks().operation()
            > before.high_water_marks().operation()
    );

    let cancelled = transaction.cancel();
    assert_eq!(cancelled.unchanged_revision, before.revision());
    assert_eq!(sketch, before);
    assert_eq!(sketch.high_water_marks(), before.high_water_marks());
}

#[test]
fn stale_trim_batch_rejects_without_overwriting_the_newer_definition() {
    let (mut sketch, target, limits) = crossing_fixture();
    let (transaction, _) = stage_two_trims(&sketch, target, limits);
    let expected = sketch.revision();
    let newer = sketch
        .stage(line((10.0, 0.0), (12.0, 0.0)), "Newer line")
        .expect("stage newer edit");
    sketch
        .commit(newer, ConfirmationSource::GreenTick)
        .expect("publish newer edit");
    let before_rejection = sketch.clone();

    assert_eq!(
        sketch
            .commit(transaction, ConfirmationSource::GreenTick)
            .expect_err("batch must retain its original optimistic revision"),
        SketchTransactionError::StaleRevision {
            expected,
            actual: SketchRevision::new(expected.get() + 1),
        }
    );
    assert_eq!(sketch, before_rejection);
}

#[test]
fn rejected_second_trim_leaves_the_first_candidate_usable() {
    let (mut sketch, target, limits) = crossing_fixture();
    let mut transaction = sketch
        .stage_trim(
            target,
            vec![limits[0], limits[1]],
            SketchPoint2::new(-1.0, 0.0),
            "Trim spans",
            PrecisionPolicy::default(),
        )
        .expect("stage first trim");
    let preview_before = transaction.preview().clone();
    let impact_before = transaction.impact().clone();

    assert!(matches!(
        transaction
            .append_trim(target, vec![limits[2]], SketchPoint2::new(3.0, 0.0))
            .expect_err("the first trim already superseded this target"),
        SketchTransactionError::Validation(SketchValidationError::MissingEntity { entity })
            if entity == target
    ));
    assert_eq!(transaction.preview(), &preview_before);
    assert_eq!(transaction.impact(), &impact_before);

    let expected_revision = SketchRevision::new(sketch.revision().get() + 1);
    sketch
        .commit(transaction, ConfirmationSource::BareEnter)
        .expect("original candidate remains committable");
    assert_eq!(sketch.revision(), expected_revision);
    assert_eq!(
        active_horizontal_ranges(&sketch),
        vec![(-4.0, -2.0), (0.0, 4.0)]
    );
}

#[test]
fn committed_trim_batch_round_trips_and_replays_deterministically() {
    let (mut sketch, target, limits) = crossing_fixture();
    let (transaction, _) = stage_two_trims(&sketch, target, limits);
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit batch");
    sketch
        .validate(PrecisionPolicy::default())
        .expect("committed batch replays");

    let encoded = serde_json::to_string(&sketch).expect("serialize batch");
    let decoded: SketchDefinition = serde_json::from_str(&encoded).expect("deserialize batch");
    decoded
        .validate(PrecisionPolicy::default())
        .expect("saved batch replays");
    assert_eq!(decoded, sketch);
    assert_eq!(
        serde_json::to_string(&decoded).expect("reserialize batch"),
        encoded
    );
}

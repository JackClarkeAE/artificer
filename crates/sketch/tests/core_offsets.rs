//! The offset recipe: a second chain that follows the first.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ChainMember, ConfirmationSource, CurveOutputRole, EvaluatedCurve2, OffsetError, PointInput,
    SignedLength, SketchDefinition, SketchEntityId, SketchOperationId, SketchPoint2, SketchRecipe,
    SketchValidationError, SketchValue, connected_chain,
};

fn point(u: f64, v: f64) -> SketchPoint2 {
    SketchPoint2::new(u, v)
}

fn line(start: (f64, f64), end: (f64, f64)) -> SketchRecipe {
    SketchRecipe::Line {
        start: PointInput::Position(point(start.0, start.1)),
        end: PointInput::Position(point(end.0, end.1)),
    }
}

fn signed(value: f64) -> SketchValue<SignedLength> {
    SketchValue::Literal(SignedLength::new(value).expect("finite distance"))
}

fn commit(sketch: &mut SketchDefinition, recipe: SketchRecipe, label: &str) -> SketchOperationId {
    let transaction = sketch.stage(recipe, label).expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    sketch.active_operations().last().expect("one operation").id
}

/// A 10 × 10 square drawn counter-clockwise as four separate lines.
fn square() -> (SketchDefinition, Vec<SketchOperationId>) {
    let mut sketch = SketchDefinition::new();
    let operations = [
        ((0.0, 0.0), (10.0, 0.0)),
        ((10.0, 0.0), (10.0, 10.0)),
        ((10.0, 10.0), (0.0, 10.0)),
        ((0.0, 10.0), (0.0, 0.0)),
    ]
    .into_iter()
    .map(|(start, end)| commit(&mut sketch, line(start, end), "side"))
    .collect();
    (sketch, operations)
}

fn sides(sketch: &SketchDefinition) -> Vec<SketchEntityId> {
    let mut entities = sketch
        .active_entities()
        .map(|entity| entity.id)
        .collect::<Vec<_>>();
    entities.sort_unstable();
    entities
}

fn offset_recipe(sketch: &SketchDefinition, seed: SketchEntityId, distance: f64) -> SketchRecipe {
    let chain = connected_chain(sketch, seed, &PrecisionPolicy::default()).expect("a chain");
    SketchRecipe::Offset {
        sources: chain.members,
        closed: chain.closed,
        distance: signed(distance),
    }
}

/// Every curve the operation published, by its semantic role.
fn published(
    sketch: &SketchDefinition,
    operation: SketchOperationId,
) -> Vec<(CurveOutputRole, SketchEntityId)> {
    let mut curves = sketch
        .active_entities()
        .filter(|entity| entity.provenance.operation == operation)
        .map(|entity| (entity.provenance.role, entity.id))
        .collect::<Vec<_>>();
    curves.sort_by_key(|(role, _)| *role);
    curves
}

fn curve(sketch: &SketchDefinition, entity: SketchEntityId) -> EvaluatedCurve2 {
    sketch.evaluated_curve(entity).expect("evaluated")
}

#[test]
fn offsetting_a_square_outward_publishes_its_four_sides_and_four_round_corners() {
    let (mut sketch, _) = square();
    let seed = sides(&sketch)[0];
    // Walked counter-clockwise, outward is to the right of travel.
    let recipe = offset_recipe(&sketch, seed, -2.0);
    let operation = commit(&mut sketch, recipe, "offset");

    let curves = published(&sketch, operation);
    assert_eq!(curves.len(), 8);
    for source in 0..4_u16 {
        assert!(
            curves
                .iter()
                .any(|(role, _)| *role == CurveOutputRole::OffsetCurve { source })
        );
        assert!(
            curves
                .iter()
                .any(|(role, _)| *role == CurveOutputRole::OffsetJoin { corner: source })
        );
    }

    // The bottom side moved 2 mm below the square it came from, and the corner
    // that follows it is a quarter arc of radius 2.
    let (_, bottom) = curves
        .iter()
        .find(|(role, _)| *role == CurveOutputRole::OffsetCurve { source: 0 })
        .copied()
        .expect("the offset of the first side");
    let EvaluatedCurve2::Line { start, end } = curve(&sketch, bottom) else {
        panic!("an offset line is a line");
    };
    assert_eq!((start.u, start.v), (0.0, -2.0));
    assert_eq!((end.u, end.v), (10.0, -2.0));

    let (_, corner) = curves
        .iter()
        .find(|(role, _)| *role == CurveOutputRole::OffsetJoin { corner: 0 })
        .copied()
        .expect("the join after the first side");
    let EvaluatedCurve2::CircularArc {
        center,
        start: arc_start,
        ..
    } = curve(&sketch, corner)
    else {
        panic!("a round join is an arc");
    };
    assert_eq!((center.u, center.v), (10.0, 0.0));
    assert!(((arc_start - center).length() - 2.0).abs() < 1.0e-12);
}

#[test]
fn editing_the_distance_keeps_every_curve_that_is_still_there() {
    let (mut sketch, _) = square();
    let seed = sides(&sketch)[0];
    let recipe = offset_recipe(&sketch, seed, -2.0);
    let operation = commit(&mut sketch, recipe, "offset");
    let before = published(&sketch, operation);

    let SketchRecipe::Offset {
        sources, closed, ..
    } = sketch
        .operation(operation)
        .expect("the offset operation")
        .recipe
        .clone()
    else {
        panic!("the operation is an offset");
    };
    let transaction = sketch
        .stage_replace(
            operation,
            SketchRecipe::Offset {
                sources,
                closed,
                distance: signed(-5.0),
            },
            "distance",
            &artificer_sketch::SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("stage the new distance");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");

    // Same roles, same entity ids: a dimension or a downstream feature that
    // referenced one of these curves still references it.
    assert_eq!(published(&sketch, operation), before);
    let (_, bottom) = before[0];
    let EvaluatedCurve2::Line { start, .. } = curve(&sketch, bottom) else {
        panic!("still a line");
    };
    assert_eq!((start.u, start.v), (0.0, -5.0));
}

#[test]
fn moving_a_source_moves_the_offset_that_came_from_it() {
    // Two sides of a corner, so the chain survives moving the far end of one.
    let mut sketch = SketchDefinition::new();
    let along = commit(&mut sketch, line((0.0, 0.0), (10.0, 0.0)), "along");
    let up = commit(&mut sketch, line((10.0, 0.0), (10.0, 10.0)), "up");
    let seed = sides(&sketch)[0];
    let recipe = offset_recipe(&sketch, seed, 2.0);
    let offset = commit(&mut sketch, recipe, "offset");
    let produced = published(&sketch, offset);
    assert_eq!(produced.len(), 2, "one concave corner, trimmed, no join");

    // Extend the upright. The offset reads its sources as evaluated curves on
    // every replay, so it has to follow.
    let transaction = sketch
        .stage_replace(
            up,
            line((10.0, 0.0), (10.0, 25.0)),
            "extend the upright",
            &artificer_sketch::SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("stage the moved side");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    assert!(sketch.operation(along).is_some());

    let (_, upright_offset) = produced
        .iter()
        .find(|(role, _)| *role == CurveOutputRole::OffsetCurve { source: 1 })
        .copied()
        .expect("the offset of the upright");
    let EvaluatedCurve2::Line { end, .. } = curve(&sketch, upright_offset) else {
        panic!("still a line");
    };
    assert!(
        (end.v - 25.0).abs() < 1.0e-9,
        "the offset follows its source to v = 25, not {}",
        end.v
    );
}

#[test]
fn an_edit_that_breaks_the_chain_refuses_rather_than_offsetting_a_gap() {
    let (mut sketch, operations) = square();
    let seed = sides(&sketch)[0];
    let recipe = offset_recipe(&sketch, seed, 2.0);
    commit(&mut sketch, recipe, "offset");

    // Lifting one side off its neighbours leaves the chain the offset was
    // taken from disconnected. There is no honest answer for what the offset
    // of a broken chain is, so the edit is refused whole.
    let refusal = sketch.stage_replace(
        operations[2],
        line((10.0, 14.0), (0.0, 14.0)),
        "raise the top",
        &artificer_sketch::SketchInputValues::default(),
        PrecisionPolicy::default(),
    );
    assert!(
        matches!(
            refusal,
            Err(artificer_sketch::SketchTransactionError::Validation(
                SketchValidationError::OffsetRefused {
                    reason: OffsetError::ChainNotConnected { .. }
                }
            ))
        ),
        "{refusal:?}"
    );
}

#[test]
fn an_offset_the_distance_eats_refuses_the_whole_transaction_by_name() {
    let (sketch, _) = square();
    let seed = sides(&sketch)[0];
    let before = sketch.active_entities().count();
    // Inward by more than half the square: every side's offset crosses the
    // one opposite it.
    let refusal = sketch.stage(offset_recipe(&sketch, seed, 6.0), "offset");
    assert!(
        matches!(
            refusal,
            Err(artificer_sketch::SketchTransactionError::Validation(
                SketchValidationError::OffsetRefused {
                    reason: OffsetError::CornerCollapses { .. }
                }
            ))
        ),
        "{refusal:?}"
    );
    assert_eq!(
        sketch.active_entities().count(),
        before,
        "a refused offset leaves the sketch exactly as it was"
    );
}

#[test]
fn an_offset_round_trips_through_the_document_and_replays_its_caches() {
    let (mut sketch, _) = square();
    let seed = sides(&sketch)[0];
    let recipe = offset_recipe(&sketch, seed, -1.5);
    commit(&mut sketch, recipe, "offset");

    let json = serde_json::to_string_pretty(&sketch).expect("serialize");
    let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, sketch);
    decoded
        .validate(PrecisionPolicy::default())
        .expect("the offset's caches replay from its recipe");

    // The chain's traversal is intent, so it is what came back.
    let operation = decoded.active_operations().last().expect("the offset").id;
    let SketchRecipe::Offset { sources, .. } = &decoded
        .operation(operation)
        .expect("the offset operation")
        .recipe
    else {
        panic!("the last operation is an offset");
    };
    assert_eq!(sources.len(), 4);
    assert!(sources.iter().all(|member: &ChainMember| !member.reversed));
}

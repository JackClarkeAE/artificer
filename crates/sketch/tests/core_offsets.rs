//! The offset recipe: a second chain that follows the first.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ChainMember, ConfirmationSource, CurveOutputRole, EvaluatedCurve2, Length, OffsetError,
    PointInput, SignedLength, SketchDefinition, SketchEntityId, SketchOperationId, SketchPoint2,
    SketchRecipe, SketchValidationError, SketchValue, connected_chain,
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

fn length(value: f64) -> SketchValue<Length> {
    SketchValue::Literal(Length::new(value).expect("positive length"))
}

/// Stages and commits a modifier — a fillet or a chamfer — over two sides.
fn commit_modifier(
    sketch: &mut SketchDefinition,
    recipe: SketchRecipe,
    label: &str,
) -> SketchOperationId {
    let transaction = sketch.stage_modifier(recipe, label).expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    sketch.active_operations().last().expect("modifier").id
}

/// The one arc among a chain's offset curves.
fn only_arc(sketch: &SketchDefinition, operation: SketchOperationId) -> (SketchPoint2, f64) {
    let arcs = published(sketch, operation)
        .into_iter()
        .filter_map(|(_, entity)| match curve(sketch, entity) {
            EvaluatedCurve2::CircularArc { center, start, .. } => {
                Some((center, (start - center).length()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(arcs.len(), 1, "exactly one arc came out");
    arcs[0]
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
fn offsetting_a_square_outward_publishes_one_curve_for_each_of_its_sides() {
    let (mut sketch, _) = square();
    let seed = sides(&sketch)[0];
    // Walked counter-clockwise, outward is to the right of travel.
    let recipe = offset_recipe(&sketch, seed, -2.0);
    let operation = commit(&mut sketch, recipe, "offset");

    let curves = published(&sketch, operation);
    assert_eq!(curves.len(), 4, "four lines in, four lines out");
    for source in 0..4_u16 {
        assert!(
            curves
                .iter()
                .any(|(role, _)| *role == CurveOutputRole::OffsetCurve { source })
        );
    }
    assert!(
        !curves
            .iter()
            .any(|(role, _)| matches!(role, CurveOutputRole::OffsetJoin { .. })),
        "a corner of two lines meets where they meet; nothing is added"
    );

    // The bottom side moved 2 mm below the square it came from, and reaches
    // the corners of the larger square rather than stopping under the old one.
    let (_, bottom) = curves
        .iter()
        .find(|(role, _)| *role == CurveOutputRole::OffsetCurve { source: 0 })
        .copied()
        .expect("the offset of the first side");
    let EvaluatedCurve2::Line { start, end } = curve(&sketch, bottom) else {
        panic!("an offset line is a line");
    };
    assert_eq!((start.u, start.v), (-2.0, -2.0));
    assert_eq!((end.u, end.v), (12.0, -2.0));
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
    assert_eq!((start.u, start.v), (-5.0, -5.0));
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

/// A filleted corner is geometry like any other: the chain walks through the
/// fillet arc, and the offset carries it across as a concentric arc rather than
/// squaring the corner off or dropping it.
#[test]
fn offsetting_a_filleted_square_keeps_the_fillet_concentric() {
    let (mut sketch, _) = square();
    let corner = sides(&sketch);
    commit_modifier(
        &mut sketch,
        SketchRecipe::Fillet {
            first: corner[0],
            second: corner[1],
            radius: length(3.0),
        },
        "fillet",
    );

    // Two sides became two trimmed fragments plus the arc between them.
    let seed = sides(&sketch)[0];
    let chain = connected_chain(&sketch, seed, &PrecisionPolicy::default()).expect("a chain");
    assert_eq!(
        chain.members.len(),
        5,
        "three sides, two fragments, one arc"
    );
    assert!(chain.closed);

    // The walk is canonical: it starts at the lowest surviving id, which the
    // fillet left as the far side, and runs the square's own way round. Left of
    // travel is therefore inward, so outward is the negative distance.
    let recipe = SketchRecipe::Offset {
        sources: chain.members,
        closed: chain.closed,
        distance: signed(-1.0),
    };
    let operation = commit(&mut sketch, recipe, "offset");
    let produced = published(&sketch, operation);
    assert_eq!(produced.len(), 5, "one curve out for each curve in");
    assert!(
        !produced
            .iter()
            .any(|(role, _)| matches!(role, CurveOutputRole::OffsetJoin { .. })),
        "a fillet's corners are tangent, so nothing is inserted at them"
    );

    let (center, radius) = only_arc(&sketch, operation);
    assert_eq!((center.u, center.v), (7.0, 3.0), "the centre does not move");
    assert!(
        (radius - 4.0).abs() < 1.0e-9,
        "an outward offset of 1 mm grows a 3 mm fillet to 4, not {radius}"
    );
}

/// The same for a chamfer: three curves become three curves, and the chamfer's
/// own offset runs parallel to it while its neighbours extend to meet it.
#[test]
fn offsetting_a_chamfered_square_keeps_the_chamfer() {
    let (mut sketch, _) = square();
    let corner = sides(&sketch);
    commit_modifier(
        &mut sketch,
        SketchRecipe::Chamfer {
            first: corner[0],
            second: corner[1],
            first_distance: length(3.0),
            second_distance: length(3.0),
        },
        "chamfer",
    );

    let seed = sides(&sketch)[0];
    let chain = connected_chain(&sketch, seed, &PrecisionPolicy::default()).expect("a chain");
    assert_eq!(
        chain.members.len(),
        5,
        "three sides, two fragments, one chamfer"
    );

    let recipe = SketchRecipe::Offset {
        sources: chain.members,
        closed: chain.closed,
        distance: signed(-1.0),
    };
    let operation = commit(&mut sketch, recipe, "offset");
    let produced = published(&sketch, operation);
    assert_eq!(produced.len(), 5, "one curve out for each curve in");
    assert!(
        produced
            .iter()
            .all(|(_, entity)| matches!(curve(&sketch, *entity), EvaluatedCurve2::Line { .. })),
        "five lines in, five lines out"
    );

    // The chamfer ran at 45° across the corner at (10, 0), from (7, 0) to
    // (10, 3). Its offset runs the same way, exactly 1 mm outside it.
    let source = (point(7.0, 0.0), point(10.0, 3.0));
    let along = (source.1 - source.0)
        .normalized()
        .expect("chamfer direction");
    let offset_chamfer = produced
        .iter()
        .filter_map(|(_, entity)| match curve(&sketch, *entity) {
            EvaluatedCurve2::Line { start, end } => {
                let direction = (end - start).normalized()?;
                (direction.cross(along).abs() < 1.0e-9 && direction.dot(along) > 0.0)
                    .then_some((start, end))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(offset_chamfer.len(), 1, "one curve is still the chamfer");
    for sample in [offset_chamfer[0].0, offset_chamfer[0].1] {
        let across = (sample - source.0).dot(along.left_normal());
        assert!(
            (across + 1.0).abs() < 1.0e-9,
            "every point of the offset chamfer is 1 mm outside the chamfer, not {across}"
        );
    }
    assert!(
        (offset_chamfer[0].1 - offset_chamfer[0].0).length() > (source.1 - source.0).length(),
        "an outward offset lengthens the chamfer"
    );
}

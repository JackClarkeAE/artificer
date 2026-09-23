//! The chains a sweep follows (ADR 0055): curves put end to end in order,
//! each turned the way the chain runs, and every way a set of curves can fail
//! to be one open, smooth path refused by name.

use artificer_sketch::{
    ChainError, ConfirmationSource, CurveDirection, PointInput, SketchDefinition, SketchEntityId,
    SketchPoint2, SketchRecipe,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

/// Draws a recipe and returns the curves it made.
fn draw(sketch: &mut SketchDefinition, recipe: SketchRecipe) -> Vec<SketchEntityId> {
    let before = sketch
        .active_entities()
        .map(|entity| entity.id)
        .collect::<Vec<_>>();
    let transaction = sketch.stage(recipe, "Draw").expect("stages");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commits");
    sketch
        .active_entities()
        .map(|entity| entity.id)
        .filter(|id| !before.contains(id))
        .collect()
}

fn line(sketch: &mut SketchDefinition, start: (f64, f64), end: (f64, f64)) -> SketchEntityId {
    draw(
        sketch,
        SketchRecipe::Line {
            start: point(start.0, start.1),
            end: point(end.0, end.1),
        },
    )[0]
}

fn arc(
    sketch: &mut SketchDefinition,
    center: (f64, f64),
    start: (f64, f64),
    end: (f64, f64),
    direction: CurveDirection,
) -> SketchEntityId {
    draw(
        sketch,
        SketchRecipe::CentreStartEndArc {
            center: point(center.0, center.1),
            start: point(start.0, start.1),
            end: point(end.0, end.1),
            direction,
        },
    )[0]
}

/// A line along +u, a quarter arc turning up, and a line up +v: tangent
/// where they meet.
fn elbow(sketch: &mut SketchDefinition) -> [SketchEntityId; 3] {
    let first = line(sketch, (0.0, 0.0), (10.0, 0.0));
    let bend = arc(
        sketch,
        (10.0, 5.0),
        (10.0, 0.0),
        (15.0, 5.0),
        CurveDirection::CounterClockwise,
    );
    let last = line(sketch, (15.0, 5.0), (15.0, 15.0));
    [first, bend, last]
}

#[test]
fn curves_named_in_any_order_come_back_end_to_end() {
    let mut sketch = SketchDefinition::new();
    let [first, bend, last] = elbow(&mut sketch);

    let chain = sketch
        .ordered_chain(&[first, bend, last])
        .expect("an elbow is a path");
    assert_eq!(
        chain.iter().map(|curve| curve.entity).collect::<Vec<_>>(),
        [first, bend, last]
    );
    assert!(chain.iter().all(|curve| !curve.reversed));
    assert_eq!(chain[0].start(), SketchPoint2::new(0.0, 0.0));
    assert_eq!(chain[2].end(), SketchPoint2::new(15.0, 15.0));

    // Named from the other end and out of order, the chain runs from the
    // free end of the curve named first, every curve turned to follow it.
    let backwards = sketch
        .ordered_chain(&[last, first, bend])
        .expect("still a path");
    assert_eq!(
        backwards
            .iter()
            .map(|curve| (curve.entity, curve.reversed))
            .collect::<Vec<_>>(),
        [(last, true), (bend, true), (first, true)]
    );
    assert_eq!(backwards[0].start(), SketchPoint2::new(15.0, 15.0));
    assert_eq!(backwards[2].end(), SketchPoint2::new(0.0, 0.0));
}

#[test]
fn a_corner_a_branch_a_gap_a_loop_and_a_circle_are_refused_by_name() {
    let mut sketch = SketchDefinition::new();
    let along = line(&mut sketch, (0.0, 0.0), (10.0, 0.0));
    let up = line(&mut sketch, (10.0, 0.0), (10.0, 10.0));
    assert_eq!(
        sketch.ordered_chain(&[along, up]),
        Err(ChainError::Corner {
            first: along,
            second: up
        })
    );
    let onward = line(&mut sketch, (10.0, 0.0), (20.0, 0.0));
    assert!(matches!(
        sketch.ordered_chain(&[along, up, onward]),
        Err(ChainError::Branch { .. })
    ));
    let apart = line(&mut sketch, (0.0, 5.0), (10.0, 5.0));
    assert_eq!(
        sketch.ordered_chain(&[along, apart]),
        Err(ChainError::Gap { entity: apart })
    );
    assert_eq!(
        sketch.ordered_chain(&[along, along]),
        Err(ChainError::Repeated { entity: along })
    );
    assert_eq!(sketch.ordered_chain(&[]), Err(ChainError::Empty));

    // Two half circles close on each other.
    let upper = arc(
        &mut sketch,
        (50.0, 0.0),
        (55.0, 0.0),
        (45.0, 0.0),
        CurveDirection::CounterClockwise,
    );
    let lower = arc(
        &mut sketch,
        (50.0, 0.0),
        (45.0, 0.0),
        (55.0, 0.0),
        CurveDirection::CounterClockwise,
    );
    assert_eq!(
        sketch.ordered_chain(&[upper, lower]),
        Err(ChainError::Closed)
    );
    let circle = draw(
        &mut sketch,
        SketchRecipe::TwoPointCircle {
            first_diameter_point: point(80.0, 0.0),
            second_diameter_point: point(90.0, 0.0),
            direction: CurveDirection::CounterClockwise,
        },
    )[0];
    assert_eq!(
        sketch.ordered_chain(&[circle]),
        Err(ChainError::ClosedCurve { entity: circle })
    );
}

#[test]
fn the_tangent_chain_through_one_curve_stops_at_a_corner() {
    let mut sketch = SketchDefinition::new();
    let [first, bend, last] = elbow(&mut sketch);
    // Carried on at a corner from the top: not part of the smooth chain.
    let corner = line(&mut sketch, (15.0, 15.0), (25.0, 15.0));
    // Apart from it altogether.
    line(&mut sketch, (0.0, 30.0), (10.0, 30.0));

    assert_eq!(sketch.tangent_chain_through(bend), vec![first, bend, last]);
    assert_eq!(sketch.tangent_chain_through(first), vec![first, bend, last]);
    // Walked from the far end, the chain comes back in its own order.
    let from_last = sketch.tangent_chain_through(last);
    assert_eq!(from_last.len(), 3);
    assert!(from_last.contains(&first) && from_last.contains(&bend));
    assert_eq!(sketch.tangent_chain_through(corner), vec![corner]);
}

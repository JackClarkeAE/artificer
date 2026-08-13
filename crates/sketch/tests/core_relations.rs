//! Relations staged and confirmed like every other sketch edit (ADR 0026, F1).
//!
//! The solver has been in the tree since the sketch crate was written, with no
//! caller. These gates pin the contract that surfaces it: a relation is an
//! atomic transaction, it moves the geometry it constrains, a conflicting
//! system is refused with the sketch left bitwise unchanged, and undo returns
//! the sketch to where it was.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ConfirmationSource, PointInput, SketchConstraintKind, SketchDefinition, SketchPoint2,
    SketchRecipe, SketchTransactionError, SketchUndoJournal,
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

/// Commits one line and returns its two point ids in recipe order.
fn commit_line(
    sketch: &mut SketchDefinition,
    start: (f64, f64),
    end: (f64, f64),
) -> (
    artificer_sketch::SketchPointId,
    artificer_sketch::SketchPointId,
) {
    let before = sketch.points().keys().copied().collect::<Vec<_>>();
    let transaction = sketch.stage(line(start, end), "Line").expect("stage line");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit line");
    let added = sketch
        .points()
        .keys()
        .copied()
        .filter(|id| !before.contains(id))
        .collect::<Vec<_>>();
    assert_eq!(added.len(), 2, "a line owns exactly two points");
    (added[0], added[1])
}

fn solved(sketch: &SketchDefinition, point: artificer_sketch::SketchPointId) -> SketchPoint2 {
    *sketch
        .solve_constraints(PrecisionPolicy::default())
        .expect("the sketch should solve")
        .positions
        .get(&point)
        .expect("the point should be solved")
}

#[test]
fn a_horizontal_relation_levels_the_line_it_names() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 4.0));
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Horizontal {
                first: start,
                second: end,
            },
            "Horizontal",
            PrecisionPolicy::default(),
        )
        .expect("a horizontal relation on a free line should stage");

    // Staging is a candidate: the live sketch is untouched until confirm.
    assert!((solved(&sketch, end).v - 4.0).abs() <= 1.0e-12);
    assert!(
        !transaction.impact().changed_points.is_empty(),
        "the solver must move something to level a slanted line"
    );

    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit the relation");
    let (first, second) = (solved(&sketch, start), solved(&sketch, end));
    assert!(
        (first.v - second.v).abs() <= 1.0e-9,
        "the line should be level, got {first:?} and {second:?}"
    );
    assert_eq!(sketch.constraints().len(), 1);
}

/// The relation must change the geometry the profile is built from, not just a
/// side table: this is the whole point of surfacing the solver.
#[test]
fn a_relation_moves_the_evaluated_curve() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 4.0));
    let entity = sketch
        .active_entities()
        .next()
        .expect("the line is active")
        .id;
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Horizontal {
                first: start,
                second: end,
            },
            "Horizontal",
            PrecisionPolicy::default(),
        )
        .expect("stage");
    assert!(
        transaction.impact().changed_entities.contains(&entity),
        "the impact must name the curve whose points moved"
    );
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let artificer_sketch::EvaluatedCurve2::Line {
        start: first,
        end: second,
    } = sketch.evaluated_curve(entity).expect("evaluate")
    else {
        panic!("a line should evaluate as a line");
    };
    assert!((first.v - second.v).abs() <= 1.0e-9);
}

#[test]
fn a_relation_the_sketch_already_satisfies_moves_nothing() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 0.0));
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Horizontal {
                first: start,
                second: end,
            },
            "Horizontal",
            PrecisionPolicy::default(),
        )
        .expect("stage");
    assert!(
        transaction.impact().changed_points.is_empty(),
        "an already level line needs no movement"
    );
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    assert_eq!(sketch.constraints().len(), 1);
}

/// Certified or refused, applied to sketching: a system that cannot converge
/// rejects the whole transaction rather than applying part of it.
#[test]
fn a_conflicting_relation_is_refused_and_leaves_the_sketch_bitwise_unchanged() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 0.0));
    let fixed_start = sketch
        .stage_constraint(
            SketchConstraintKind::Fixed {
                point: start,
                position: SketchPoint2::new(0.0, 0.0),
            },
            "Fixed",
            PrecisionPolicy::default(),
        )
        .expect("stage the first anchor");
    sketch
        .commit(fixed_start, ConfirmationSource::GreenTick)
        .expect("commit the first anchor");
    let fixed_end = sketch
        .stage_constraint(
            SketchConstraintKind::Fixed {
                point: end,
                position: SketchPoint2::new(10.0, 0.0),
            },
            "Fixed",
            PrecisionPolicy::default(),
        )
        .expect("stage the second anchor");
    sketch
        .commit(fixed_end, ConfirmationSource::GreenTick)
        .expect("commit the second anchor");

    let before = sketch.clone();
    // Both endpoints are pinned ten apart; demanding one is a contradiction.
    let error = sketch
        .stage_constraint(
            SketchConstraintKind::Distance {
                first: start,
                second: end,
                distance: 1.0,
            },
            "Distance",
            PrecisionPolicy::default(),
        )
        .expect_err("a contradictory distance must refuse");
    assert!(
        matches!(error, SketchTransactionError::ConstraintRejected(_)),
        "unexpected refusal: {error:?}"
    );
    assert_eq!(sketch, before, "a refused relation must change nothing");
}

#[test]
fn a_relation_naming_a_missing_point_is_refused() {
    let mut sketch = SketchDefinition::new();
    let (start, _) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 0.0));
    let error = sketch
        .stage_constraint(
            SketchConstraintKind::Coincident {
                first: start,
                second: artificer_sketch::SketchPointId::new(9_999).expect("a valid id"),
            },
            "Coincident",
            PrecisionPolicy::default(),
        )
        .expect_err("an unknown point must refuse");
    assert!(matches!(
        error,
        SketchTransactionError::ConstraintRejected(_)
    ));
}

#[test]
fn undo_returns_the_sketch_to_life_before_the_relation() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 4.0));
    let mut journal = SketchUndoJournal::new(8);
    let before = sketch.clone();
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Horizontal {
                first: start,
                second: end,
            },
            "Horizontal",
            PrecisionPolicy::default(),
        )
        .expect("stage");
    journal
        .confirm(
            &mut sketch,
            transaction,
            ConfirmationSource::GreenTick,
            PrecisionPolicy::default(),
        )
        .expect("confirm through the journal");
    assert_eq!(sketch.constraints().len(), 1);
    assert!(journal.undo(&mut sketch), "undo should restore");
    assert!(
        sketch.constraints().is_empty(),
        "undo must remove the relation"
    );
    assert_eq!(
        sketch.revision(),
        before.revision(),
        "undo must restore the revision"
    );
    assert_eq!(
        sketch.points(),
        before.points(),
        "undo must restore the geometry"
    );
    // Identifiers are deliberately not recycled: the high-water mark keeps the
    // retired constraint's id spent, so a later relation cannot reuse it.
    assert!(
        sketch
            .stage_constraint(
                SketchConstraintKind::Horizontal {
                    first: start,
                    second: end,
                },
                "Horizontal",
                PrecisionPolicy::default(),
            )
            .is_ok(),
        "the sketch stays usable after undo"
    );
}

/// Two lines, one relation between them: the pairwise kinds work on the point
/// ids the curves already own, with no new geometry anywhere.
#[test]
fn a_perpendicular_relation_squares_two_lines() {
    let mut sketch = SketchDefinition::new();
    let (first_start, first_end) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 0.0));
    let (second_start, second_end) = commit_line(&mut sketch, (0.0, 0.0), (6.0, 3.0));
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Perpendicular {
                first_start,
                first_end,
                second_start,
                second_end,
            },
            "Perpendicular",
            PrecisionPolicy::default(),
        )
        .expect("stage perpendicular");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let (a, b) = (solved(&sketch, first_start), solved(&sketch, first_end));
    let (c, d) = (solved(&sketch, second_start), solved(&sketch, second_end));
    let dot = (b.u - a.u).mul_add(d.u - c.u, (b.v - a.v) * (d.v - c.v));
    assert!(
        dot.abs() <= 1.0e-7,
        "the lines should be square, dot = {dot}"
    );
}

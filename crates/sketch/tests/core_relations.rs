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

/// A relation can be released again, through the same staged path that made
/// it. The equation goes; the geometry returns to what its recipe says,
/// because a relation is a projection over the recipes rather than an edit
/// written back into them.
#[test]
fn releasing_a_relation_stages_like_making_one_and_returns_the_geometry() {
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
        .expect("stage the relation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit the relation");
    let id = *sketch.constraints().keys().next().expect("one relation");
    let held = sketch.revision();
    assert!((solved(&sketch, end).v - solved(&sketch, start).v).abs() <= 1.0e-9);

    let removal = sketch
        .stage_constraint_removal(id, "Remove relation", PrecisionPolicy::default())
        .expect("releasing a relation should stage");
    assert!(
        !removal.impact().changed_points.is_empty(),
        "releasing a relation that was holding the line must report the movement"
    );
    // Staging changes nothing: the live sketch still holds the relation.
    assert_eq!(sketch.constraints().len(), 1);
    assert_eq!(sketch.revision(), held);

    sketch
        .commit(removal, ConfirmationSource::GreenTick)
        .expect("commit the release");
    assert!(sketch.constraints().is_empty());
    assert!(
        (solved(&sketch, end).v - 4.0).abs() <= 1.0e-9,
        "the released line returns to the shape it was drawn with"
    );

    // Undo puts a released relation back, the same way it takes a made one
    // away: the journal is what a user reaches for after releasing the wrong
    // one.
    let mut journal = SketchUndoJournal::new(8);
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Horizontal {
                first: start,
                second: end,
            },
            "Horizontal",
            PrecisionPolicy::default(),
        )
        .expect("stage the relation again");
    journal
        .confirm(
            &mut sketch,
            transaction,
            ConfirmationSource::GreenTick,
            PrecisionPolicy::default(),
        )
        .expect("confirm the relation through the journal");
    let reinstated = *sketch.constraints().keys().next().expect("one relation");
    let removal = sketch
        .stage_constraint_removal(reinstated, "Remove relation", PrecisionPolicy::default())
        .expect("stage the release");
    journal
        .confirm(
            &mut sketch,
            removal,
            ConfirmationSource::GreenTick,
            PrecisionPolicy::default(),
        )
        .expect("confirm the release through the journal");
    assert!(sketch.constraints().is_empty());

    assert!(journal.undo(&mut sketch), "the release must be undoable");
    assert_eq!(sketch.constraints().len(), 1);
    assert!(
        (solved(&sketch, end).v - solved(&sketch, start).v).abs() <= 1.0e-9,
        "undoing a release puts the line back under the relation"
    );
}

/// Releasing a relation that is not there changes nothing, and says so rather
/// than publishing an empty revision.
#[test]
fn releasing_an_absent_relation_is_refused_as_no_change() {
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
        .expect("stage the relation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit the relation");
    let id = *sketch.constraints().keys().next().expect("one relation");
    let removal = sketch
        .stage_constraint_removal(id, "Remove relation", PrecisionPolicy::default())
        .expect("stage the release");
    sketch
        .commit(removal, ConfirmationSource::GreenTick)
        .expect("commit the release");

    assert!(matches!(
        sketch.stage_constraint_removal(id, "Remove relation", PrecisionPolicy::default()),
        Err(SketchTransactionError::NoChange)
    ));
}

/// A hole placed in a plate, which is the dimension a drawer actually reaches
/// for: the circle's centre held a typed distance from two edges of the
/// rectangle around it.
///
/// Both shapes are recipes, so both move as bodies: the circle translates to
/// meet the dimension and stays a circle, and the rectangle stays a rectangle.
#[test]
fn a_circle_is_placed_in_a_rectangle_by_dimensions_from_two_edges() {
    let mut sketch = SketchDefinition::new();
    // A 40 x 20 plate with its lower-left corner at the origin.
    let plate = sketch
        .stage(
            SketchRecipe::TwoPointRectangle {
                first_corner: point(0.0, 0.0),
                width: artificer_sketch::SketchValue::Literal(
                    artificer_sketch::SignedLength::new(40.0).expect("width"),
                ),
                height: artificer_sketch::SketchValue::Literal(
                    artificer_sketch::SignedLength::new(20.0).expect("height"),
                ),
            },
            "Plate",
        )
        .expect("stage plate");
    sketch
        .commit(plate, ConfirmationSource::GreenTick)
        .expect("commit plate");
    let plate_points = sketch.points().keys().copied().collect::<Vec<_>>();
    let corner_at = |sketch: &SketchDefinition, u: f64, v: f64| {
        *sketch
            .points()
            .iter()
            .find(|(_, record)| {
                (record.evaluated_position.u - u).abs() < 1.0e-9
                    && (record.evaluated_position.v - v).abs() < 1.0e-9
            })
            .expect("corner")
            .0
    };
    let bottom_left = corner_at(&sketch, 0.0, 0.0);
    let bottom_right = corner_at(&sketch, 40.0, 0.0);
    let top_left = corner_at(&sketch, 0.0, 20.0);

    // A circle somewhere in the middle, drawn by eye.
    let hole = sketch
        .stage(
            SketchRecipe::CentrePointCircle {
                center: point(17.0, 11.0),
                radius: artificer_sketch::SketchValue::Literal(
                    artificer_sketch::Length::new(3.0).expect("radius"),
                ),
                radial_angle: artificer_sketch::SketchValue::Literal(
                    artificer_sketch::Angle::radians(0.0).expect("angle"),
                ),
            },
            "Hole",
        )
        .expect("stage hole");
    sketch
        .commit(hole, ConfirmationSource::GreenTick)
        .expect("commit hole");
    let centre = *sketch
        .points()
        .keys()
        .find(|id| !plate_points.contains(id))
        .expect("the circle's centre is a point of its own");

    // 12 from the left edge, 8 up from the bottom edge. The left edge runs
    // from the bottom-left corner up; the bottom edge runs to the right.
    for (kind, what) in [
        (
            SketchConstraintKind::PointToLineDistance {
                point: centre,
                line_start: bottom_left,
                line_end: top_left,
                distance: -12.0,
            },
            "12 from the left edge",
        ),
        (
            SketchConstraintKind::PointToLineDistance {
                point: centre,
                line_start: bottom_left,
                line_end: bottom_right,
                distance: 8.0,
            },
            "8 up from the bottom edge",
        ),
    ] {
        let transaction = sketch
            .stage_constraint(kind, what, PrecisionPolicy::default())
            .unwrap_or_else(|error| panic!("{what} should stage: {error}"));
        sketch
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap_or_else(|error| panic!("{what} should commit: {error}"));
    }

    let placed = solved(&sketch, centre);
    assert!(
        (placed.u - 12.0).abs() <= 1.0e-9 && (placed.v - 8.0).abs() <= 1.0e-9,
        "the hole should sit where the two dimensions put it, got {placed:?}"
    );
    // The plate did not move, and it is still a plate.
    assert_eq!(solved(&sketch, bottom_left), SketchPoint2::new(0.0, 0.0));
    assert_eq!(solved(&sketch, bottom_right), SketchPoint2::new(40.0, 0.0));
    assert_eq!(solved(&sketch, top_left), SketchPoint2::new(0.0, 20.0));
}

/// Where a dimension's value sits is part of the drawing, so it travels with
/// the document — and it is not a change to the geometry, so it does not
/// advance the revision and cannot make a feature built on this sketch stale.
#[test]
fn a_dimension_label_offset_persists_without_advancing_the_revision() {
    let mut sketch = SketchDefinition::new();
    let (start, _) = commit_line(&mut sketch, (0.0, 0.0), (10.0, 0.0));
    let (other, _) = commit_line(&mut sketch, (0.0, 4.0), (10.0, 4.0));
    let transaction = sketch
        .stage_constraint(
            SketchConstraintKind::Distance {
                first: start,
                second: other,
                distance: 4.0,
            },
            "Distance",
            PrecisionPolicy::default(),
        )
        .expect("stage the dimension");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit the dimension");
    let id = *sketch.constraints().keys().next().expect("one dimension");
    let revision = sketch.revision();

    let offset = SketchPoint2::new(-3.0, 1.5);
    assert!(sketch.set_constraint_label_offset(id, Some(offset)));
    assert_eq!(sketch.constraints()[&id].label_offset, Some(offset));
    assert_eq!(
        sketch.revision(),
        revision,
        "moving a label is not a change to the geometry"
    );
    assert!(
        !sketch.set_constraint_label_offset(id, Some(offset)),
        "putting a label where it already is changes nothing"
    );
    assert!(
        !sketch.set_constraint_label_offset(id, Some(SketchPoint2::new(f64::NAN, 0.0))),
        "a label cannot be moved somewhere unpaintable"
    );
    assert_eq!(sketch.constraints()[&id].label_offset, Some(offset));

    // It survives the trip through the document.
    let json = serde_json::to_string(&sketch).expect("serialize");
    let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded.constraints()[&id].label_offset, Some(offset));

    // And retyping the dimension keeps the label where it was put.
    let retyped = sketch
        .stage_constraint_value(id, 6.0, "Sketch dimension", PrecisionPolicy::default())
        .expect("stage the new value");
    sketch
        .commit(retyped, ConfirmationSource::GreenTick)
        .expect("commit the new value");
    assert_eq!(sketch.constraints()[&id].label_offset, Some(offset));
    assert_eq!(sketch.constraints()[&id].kind.value(), Some(6.0));

    // A sketch written before labels could be moved reads back with none.
    let legacy = json.replace(
        r#""label_offset":{"u":-3.0,"v":1.5}"#,
        r#""label_offset":null"#,
    );
    let decoded: SketchDefinition = serde_json::from_str(&legacy).expect("deserialize");
    assert_eq!(decoded.constraints()[&id].label_offset, None);
}

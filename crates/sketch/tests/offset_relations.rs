//! Dimensions that measure to an edge rather than to one of its corners.
//!
//! A distance to a corner is a radius: it leaves the point anywhere on a
//! circle, two of them meet in two places or in none, and neither of those is
//! what a drawing means by "twenty from that edge". These gates pin the three
//! relations that say it properly — an offset from an edge, a distance to an
//! edge's midpoint, and the separation of two parallel edges — and pin that an
//! offset behaves the way an ordinate has to: two of them, from two edges,
//! land the point in exactly one place.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ConfirmationSource, ConstraintError, PointInput, SketchConstraintKind, SketchDefinition,
    SketchPoint2, SketchPointId, SketchRecipe, SketchTransactionError, dimension_span,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn commit(sketch: &mut SketchDefinition, recipe: SketchRecipe, label: &str) -> Vec<SketchPointId> {
    let before = sketch.points().keys().copied().collect::<Vec<_>>();
    let transaction = sketch.stage(recipe, label).expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    sketch
        .points()
        .keys()
        .copied()
        .filter(|id| !before.contains(id))
        .collect()
}

fn line(
    sketch: &mut SketchDefinition,
    start: (f64, f64),
    end: (f64, f64),
) -> (SketchPointId, SketchPointId) {
    let added = commit(
        sketch,
        SketchRecipe::Line {
            start: point(start.0, start.1),
            end: point(end.0, end.1),
        },
        "Line",
    );
    (added[0], added[1])
}

fn free_point(sketch: &mut SketchDefinition, at: (f64, f64)) -> SketchPointId {
    commit(
        sketch,
        SketchRecipe::Point {
            position: SketchPoint2::new(at.0, at.1),
        },
        "Point",
    )[0]
}

fn add(
    sketch: &mut SketchDefinition,
    kind: SketchConstraintKind,
) -> Result<(), SketchTransactionError> {
    let staged = sketch.stage_constraint(kind, "Relation", PrecisionPolicy::default())?;
    sketch.commit(staged, ConfirmationSource::GreenTick)?;
    Ok(())
}

fn solved(sketch: &SketchDefinition, id: SketchPointId) -> SketchPoint2 {
    *sketch
        .solve_constraints(PrecisionPolicy::default())
        .expect("the sketch should solve")
        .positions
        .get(&id)
        .expect("the point should be solved")
}

fn pin(sketch: &mut SketchDefinition, id: SketchPointId, at: (f64, f64)) {
    add(
        sketch,
        SketchConstraintKind::Fixed {
            point: id,
            position: SketchPoint2::new(at.0, at.1),
        },
    )
    .expect("pin");
}

/// The offset is from the edge's *line*, along its normal — not from either of
/// its corners. A point twenty above a horizontal edge is twenty above it
/// wherever along it the point sits.
#[test]
fn an_offset_holds_a_point_off_an_edges_line() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    pin(&mut sketch, start, (0.0, 0.0));
    pin(&mut sketch, end, (100.0, 0.0));
    let hole = free_point(&mut sketch, (30.0, 5.0));

    add(
        &mut sketch,
        SketchConstraintKind::PointToLineDistance {
            point: hole,
            start,
            end,
            distance: 20.0,
        },
    )
    .expect("twenty off a horizontal edge is satisfiable");

    let at = solved(&sketch, hole);
    assert!(
        (at.v - 20.0).abs() <= 1.0e-9,
        "the point should sit twenty above the edge, and sits at {at:?}"
    );
    assert!(
        (at.u - 30.0).abs() <= 1.0e-9,
        "an offset says nothing about where along the edge, so u must not move: {at:?}"
    );
}

/// The whole point of an ordinate. Two offsets, from two edges at right
/// angles, put the point in exactly one place — no mirror twin to choose
/// between, and no pair of radii that might not reach.
#[test]
fn two_offsets_from_two_edges_position_a_point_exactly() {
    let mut sketch = SketchDefinition::new();
    let (bottom_start, bottom_end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    let (left_start, left_end) = line(&mut sketch, (0.0, 0.0), (0.0, 100.0));
    for (id, at) in [
        (bottom_start, (0.0, 0.0)),
        (bottom_end, (100.0, 0.0)),
        (left_start, (0.0, 0.0)),
        (left_end, (0.0, 100.0)),
    ] {
        pin(&mut sketch, id, at);
    }
    let hole = free_point(&mut sketch, (5.0, 5.0));

    add(
        &mut sketch,
        SketchConstraintKind::PointToLineDistance {
            point: hole,
            start: bottom_start,
            end: bottom_end,
            distance: 20.0,
        },
    )
    .expect("twenty up");
    add(
        &mut sketch,
        SketchConstraintKind::PointToLineDistance {
            point: hole,
            start: left_start,
            end: left_end,
            distance: 35.0,
        },
    )
    .expect("thirty-five across");

    let at = solved(&sketch, hole);
    assert!(
        (at.u - 35.0).abs() <= 1.0e-9 && (at.v - 20.0).abs() <= 1.0e-9,
        "two ordinates should land the point at (35, 20), and it is at {at:?}"
    );
}

/// An offset keeps the side the point is already on, so retyping a dimension
/// moves the point along the normal rather than flipping it through the edge
/// to the other side of the material.
#[test]
fn an_offset_does_not_flip_a_point_through_the_edge() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    pin(&mut sketch, start, (0.0, 0.0));
    pin(&mut sketch, end, (100.0, 0.0));
    let below = free_point(&mut sketch, (30.0, -5.0));

    add(
        &mut sketch,
        SketchConstraintKind::PointToLineDistance {
            point: below,
            start,
            end,
            distance: 20.0,
        },
    )
    .expect("twenty off the edge");

    let at = solved(&sketch, below);
    assert!(
        (at.v + 20.0).abs() <= 1.0e-9,
        "a point below the edge stays below it, and this one is at {at:?}"
    );
}

/// The midpoint of an edge is not a point the sketch owns, so the relation
/// names the edge's ends and measures to the middle of them.
#[test]
fn a_distance_to_an_edges_midpoint_measures_to_the_middle_of_it() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    pin(&mut sketch, start, (0.0, 0.0));
    pin(&mut sketch, end, (100.0, 0.0));
    let hole = free_point(&mut sketch, (50.0, 5.0));

    add(
        &mut sketch,
        SketchConstraintKind::PointToMidpointDistance {
            point: hole,
            start,
            end,
            distance: 30.0,
        },
    )
    .expect("thirty from the middle");

    let at = solved(&sketch, hole);
    let middle = SketchPoint2::new(50.0, 0.0);
    let measured = (at.u - middle.u).hypot(at.v - middle.v);
    assert!(
        (measured - 30.0).abs() <= 1.0e-9,
        "the point should be thirty from (50, 0), and measures {measured} at {at:?}"
    );
}

/// Two parallel edges stand a distance apart, and the relation moves the
/// second edge bodily rather than tilting it.
#[test]
fn two_parallel_edges_stand_the_distance_apart() {
    let mut sketch = SketchDefinition::new();
    let (bottom_start, bottom_end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    let (top_start, top_end) = line(&mut sketch, (0.0, 10.0), (100.0, 10.0));
    pin(&mut sketch, bottom_start, (0.0, 0.0));
    pin(&mut sketch, bottom_end, (100.0, 0.0));

    add(
        &mut sketch,
        SketchConstraintKind::LineToLineDistance {
            first_start: bottom_start,
            first_end: bottom_end,
            second_start: top_start,
            second_end: top_end,
            distance: 42.0,
        },
    )
    .expect("forty-two apart");

    let (first, second) = (solved(&sketch, top_start), solved(&sketch, top_end));
    assert!(
        (first.v - 42.0).abs() <= 1.0e-9 && (second.v - 42.0).abs() <= 1.0e-9,
        "the top edge should lie at v = 42 along its whole length: {first:?} {second:?}"
    );
    assert!(
        (first.u - 0.0).abs() <= 1.0e-9 && (second.u - 100.0).abs() <= 1.0e-9,
        "the edge moves across, not along: {first:?} {second:?}"
    );
}

/// A dimension has to know where to draw its witness line, and for an offset
/// that is the foot of the perpendicular rather than either corner.
#[test]
fn an_offsets_span_runs_to_the_foot_of_the_perpendicular() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    let hole = free_point(&mut sketch, (30.0, 20.0));
    let kind = SketchConstraintKind::PointToLineDistance {
        point: hole,
        start,
        end,
        distance: 20.0,
    };
    let positions = sketch
        .solve_constraints(PrecisionPolicy::default())
        .expect("solves")
        .positions;

    let (from, to) = dimension_span(&kind, &positions).expect("an offset has a span");
    assert!(
        (from.u - 30.0).abs() <= 1.0e-9 && (from.v - 20.0).abs() <= 1.0e-9,
        "the span starts at the point: {from:?}"
    );
    assert!(
        (to.u - 30.0).abs() <= 1.0e-9 && to.v.abs() <= 1.0e-9,
        "and ends at its foot on the edge, not at a corner: {to:?}"
    );
}

/// Zero is not a dimension. A point *on* a line is a collinear relation and
/// two lines on top of one another are coincident; both leave the normal these
/// project along undefined, so both are refused by name.
#[test]
fn an_offset_of_zero_is_refused() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    let hole = free_point(&mut sketch, (30.0, 20.0));
    let before = sketch.clone();
    let error = add(
        &mut sketch,
        SketchConstraintKind::PointToLineDistance {
            point: hole,
            start,
            end,
            distance: 0.0,
        },
    )
    .expect_err("an offset of zero must refuse");
    assert!(
        matches!(
            error,
            SketchTransactionError::ConstraintRejected(ConstraintError::NonPositiveDistance)
        ),
        "unexpected refusal: {error:?}"
    );
    assert_eq!(sketch, before, "a refused relation changes nothing");
}

/// Every one of these carries a number, so every one of them can be retyped by
/// the dimension tool. A relation that could not would be drawn as a dimension
/// and then refuse to be edited, which is worse than not drawing it.
#[test]
fn each_offset_relation_carries_a_measurement_the_dimension_tool_can_retype() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    let (other_start, other_end) = line(&mut sketch, (0.0, 10.0), (100.0, 10.0));
    let hole = free_point(&mut sketch, (30.0, 20.0));
    for kind in [
        SketchConstraintKind::PointToLineDistance {
            point: hole,
            start,
            end,
            distance: 20.0,
        },
        SketchConstraintKind::PointToMidpointDistance {
            point: hole,
            start,
            end,
            distance: 20.0,
        },
        SketchConstraintKind::LineToLineDistance {
            first_start: start,
            first_end: end,
            second_start: other_start,
            second_end: other_end,
            distance: 10.0,
        },
    ] {
        assert!(
            kind.measurement().is_some(),
            "{kind:?} should report the number it holds"
        );
        let restated = kind.with_measurement(7.5).expect("it should restate");
        assert_eq!(
            restated.measurement(),
            Some(7.5),
            "{kind:?} should hold the number it was restated with"
        );
        assert_eq!(
            restated.referenced_points(),
            kind.referenced_points(),
            "restating a relation must not change what it names"
        );
    }
}

/// Retyping an offset moves the thing being located and leaves the edge it is
/// measured from alone — including the far end of that edge, which a single
/// anchor would let the projection tilt.
#[test]
fn retyping_an_offset_moves_the_point_and_leaves_the_edge_whole() {
    let mut sketch = SketchDefinition::new();
    let (start, end) = line(&mut sketch, (0.0, 0.0), (100.0, 0.0));
    let hole = free_point(&mut sketch, (30.0, 20.0));
    add(
        &mut sketch,
        SketchConstraintKind::PointToLineDistance {
            point: hole,
            start,
            end,
            distance: 20.0,
        },
    )
    .expect("twenty off the edge");
    let constraint = *sketch.constraints().keys().next().expect("one relation");

    let staged = sketch
        .stage_relation_measurement(
            constraint,
            45.0,
            None,
            "Dimension",
            PrecisionPolicy::default(),
        )
        .expect("forty-five is a distance this sketch can hold");
    sketch
        .commit(staged, ConfirmationSource::GreenTick)
        .expect("commit the dimension");

    let (a, b) = (solved(&sketch, start), solved(&sketch, end));
    assert!(
        a.u.abs() <= 1.0e-9
            && a.v.abs() <= 1.0e-9
            && (b.u - 100.0).abs() <= 1.0e-9
            && b.v.abs() <= 1.0e-9,
        "the edge is the datum and must not move or tilt: {a:?} {b:?}"
    );
    let at = solved(&sketch, hole);
    assert!(
        (at.v - 45.0).abs() <= 1.0e-9,
        "the point should have moved out to forty-five, and is at {at:?}"
    );
}

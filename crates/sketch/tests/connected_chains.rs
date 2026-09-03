//! Walking the connected chain a click on one curve means.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    Angle, ChainError, ConfirmationSource, Length, PointInput, SketchDefinition, SketchEntityId,
    SketchPoint2, SketchRecipe, SketchValue, chain_geometry, connected_chain, offset_chain,
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

fn centreline(start: (f64, f64), end: (f64, f64)) -> SketchRecipe {
    SketchRecipe::CentreLine {
        start: PointInput::Position(point(start.0, start.1)),
        end: PointInput::Position(point(end.0, end.1)),
    }
}

/// Commits one recipe and returns the entities it published, in order.
fn commit(sketch: &mut SketchDefinition, recipe: SketchRecipe, label: &str) -> Vec<SketchEntityId> {
    let before = sketch
        .active_entities()
        .map(|entity| entity.id)
        .collect::<Vec<_>>();
    let transaction = sketch.stage(recipe, label).expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let mut published = sketch
        .active_entities()
        .map(|entity| entity.id)
        .filter(|entity| !before.contains(entity))
        .collect::<Vec<_>>();
    published.sort_unstable();
    published
}

/// Three sides of a square, drawn as separate lines in a scrambled order and
/// with two of them running backwards. Connectivity is a property of the
/// geometry, not of the order somebody happened to draw it in.
fn scrambled_open_chain() -> (SketchDefinition, Vec<SketchEntityId>) {
    let mut sketch = SketchDefinition::new();
    let middle = commit(&mut sketch, line((10.0, 0.0), (10.0, 10.0)), "middle");
    let first = commit(&mut sketch, line((10.0, 0.0), (0.0, 0.0)), "first");
    let last = commit(&mut sketch, line((0.0, 10.0), (10.0, 10.0)), "last");
    let entities = vec![middle[0], first[0], last[0]];
    (sketch, entities)
}

fn closed_square() -> (SketchDefinition, Vec<SketchEntityId>) {
    let mut sketch = SketchDefinition::new();
    let mut entities = Vec::new();
    for (start, end) in [
        ((0.0, 0.0), (10.0, 0.0)),
        ((10.0, 0.0), (10.0, 10.0)),
        ((10.0, 10.0), (0.0, 10.0)),
        ((0.0, 10.0), (0.0, 0.0)),
    ] {
        entities.extend(commit(&mut sketch, line(start, end), "side"));
    }
    (sketch, entities)
}

#[test]
fn a_click_on_any_side_of_a_square_walks_the_whole_loop_the_same_way() {
    let (sketch, entities) = closed_square();
    let precision = PrecisionPolicy::default();
    let from_first = connected_chain(&sketch, entities[0], &precision).expect("a chain");
    assert!(from_first.closed);
    assert_eq!(from_first.members.len(), 4);
    // The walk is canonical, so every seed yields the same loop in the same
    // order: a recipe keyed on chain position must not depend on the click.
    for seed in &entities {
        let chain = connected_chain(&sketch, *seed, &precision).expect("a chain");
        assert_eq!(chain, from_first, "seeding from {seed:?} changed the walk");
    }
    assert_eq!(
        from_first
            .members
            .iter()
            .map(|member| member.entity)
            .collect::<Vec<_>>(),
        entities
    );
    assert!(from_first.members.iter().all(|member| !member.reversed));
}

#[test]
fn an_open_chain_reads_head_to_tail_however_its_curves_were_drawn() {
    let (sketch, entities) = scrambled_open_chain();
    let precision = PrecisionPolicy::default();
    let chain = connected_chain(&sketch, entities[0], &precision).expect("a chain");
    assert!(!chain.closed);
    assert_eq!(chain.members.len(), 3);

    // Whatever order the walk chose, the geometry it hands the offset must
    // read head to tail: that is the whole point of carrying `reversed`.
    let geometry = chain_geometry(&sketch, &chain).expect("geometry");
    assert_eq!(geometry.curves.len(), 3);
    assert!(!geometry.closed);
    // An offset accepts it, which is the connectivity check in the one place
    // that matters.
    let offset = offset_chain(&geometry, 1.0, &precision).expect("a connected chain offsets");
    assert!(offset.len() >= 3);

    // And every seed gives the same chain.
    for seed in &entities {
        assert_eq!(
            connected_chain(&sketch, *seed, &precision).expect("a chain"),
            chain
        );
    }
}

#[test]
fn a_junction_where_three_curves_meet_stops_the_walk_rather_than_guessing() {
    let mut sketch = SketchDefinition::new();
    let stem = commit(&mut sketch, line((0.0, 0.0), (10.0, 0.0)), "stem");
    // Two branches leaving the same point: there is no single continuation.
    commit(&mut sketch, line((10.0, 0.0), (10.0, 10.0)), "up");
    commit(&mut sketch, line((10.0, 0.0), (10.0, -10.0)), "down");

    let chain = connected_chain(&sketch, stem[0], &PrecisionPolicy::default()).expect("a chain");
    assert_eq!(
        chain
            .members
            .iter()
            .map(|member| member.entity)
            .collect::<Vec<_>>(),
        stem,
        "a T-junction ends the chain at the stem it was seeded from"
    );
    assert!(!chain.closed);
}

#[test]
fn a_centreline_is_not_swept_into_an_outline_that_happens_to_touch_it() {
    let mut sketch = SketchDefinition::new();
    let side = commit(&mut sketch, line((0.0, 0.0), (10.0, 0.0)), "side");
    let construction = commit(
        &mut sketch,
        centreline((10.0, 0.0), (10.0, 10.0)),
        "centreline",
    );

    let precision = PrecisionPolicy::default();
    let chain = connected_chain(&sketch, side[0], &precision).expect("a chain");
    assert_eq!(
        chain.members.len(),
        1,
        "construction geometry is not outline"
    );
    assert_eq!(
        connected_chain(&sketch, construction[0], &precision),
        Err(ChainError::UnsupportedSeed {
            entity: construction[0]
        })
    );
}

#[test]
fn a_circle_is_a_closed_chain_of_itself() {
    let mut sketch = SketchDefinition::new();
    let circle = commit(
        &mut sketch,
        SketchRecipe::CentrePointCircle {
            center: PointInput::Position(point(0.0, 0.0)),
            radius: SketchValue::Literal(Length::new(3.0).expect("radius")),
            radial_angle: SketchValue::Literal(Angle::radians(0.0).expect("angle")),
        },
        "circle",
    );
    let chain =
        connected_chain(&sketch, circle[0], &PrecisionPolicy::default()).expect("a chain of one");
    assert!(chain.closed);
    assert_eq!(chain.members.len(), 1);
}

#[test]
fn a_curve_that_is_not_in_the_sketch_is_refused_by_name() {
    let (sketch, _) = closed_square();
    let stranger = SketchEntityId::new(9_999).expect("id");
    assert_eq!(
        connected_chain(&sketch, stranger, &PrecisionPolicy::default()),
        Err(ChainError::MissingSeed { entity: stranger })
    );
}

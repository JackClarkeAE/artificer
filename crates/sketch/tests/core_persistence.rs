use artificer_protocol::{
    ArcDirection, PlanarCurve2, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, PrecisionPolicy,
};
use artificer_sketch::{
    ConfirmationSource, CurveOutputRole, OutputRole, PointInput, RetirementPolicy, SketchCurve2,
    SketchDefinition, SketchPoint2, SketchRecipe, SketchRevision, SketchValidationError,
};

fn line() -> SketchRecipe {
    SketchRecipe::Line {
        start: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
        end: PointInput::Position(SketchPoint2::new(4.0, 0.0)),
    }
}

#[test]
fn structurally_valid_but_tampered_evaluated_cache_is_rejected() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch.stage(line(), "Line").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let mut json = serde_json::to_value(&sketch).expect("serialize graph");
    let first_point = json["points"]
        .as_object_mut()
        .and_then(|points| points.values_mut().next())
        .expect("one point cache");
    first_point["evaluated_position"]["v"] = serde_json::json!(1.0);
    let tampered: SketchDefinition = serde_json::from_value(json).expect("decode graph");
    assert!(matches!(
        tampered.validate(PrecisionPolicy::default()),
        Err(SketchValidationError::EvaluatedCacheMismatch { .. })
    ));
}

#[test]
fn editable_definition_round_trips_with_ids_recipes_and_high_water_marks() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch.stage(line(), "Line").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let json = serde_json::to_string_pretty(&sketch).expect("serialize graph");
    let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize graph");
    assert_eq!(decoded, sketch);
    decoded
        .validate(PrecisionPolicy::default())
        .expect("checked caches reconstruct coherently");
}

#[test]
fn confirmed_deletion_round_trips_as_inactive_tombstones() {
    let mut sketch = SketchDefinition::new();
    let insert = sketch.stage(line(), "Line").expect("stage line");
    sketch
        .commit(insert, ConfirmationSource::GreenTick)
        .expect("commit line");
    let operation = sketch.operations()[0].id;
    let entity = sketch.active_entities().next().expect("active line").id;
    let published = sketch.high_water_marks();

    let delete = sketch
        .stage_retire_operation(
            operation,
            RetirementPolicy::CascadeDependents,
            "Delete sketch geometry",
            PrecisionPolicy::default(),
        )
        .expect("stage deletion");
    sketch
        .commit(delete, ConfirmationSource::BareEnter)
        .expect("confirm deletion");

    let json = serde_json::to_string_pretty(&sketch).expect("serialize deleted graph");
    let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize deleted graph");
    assert!(
        !decoded
            .operation(operation)
            .expect("operation tombstone")
            .active
    );
    assert!(!decoded.entity(entity).expect("curve tombstone").active);
    assert_eq!(decoded.active_operations().count(), 0);
    assert_eq!(decoded.active_entities().count(), 0);
    assert_eq!(decoded.high_water_marks(), published);
    decoded
        .validate(PrecisionPolicy::default())
        .expect("deleted graph remains persistently valid");
}

#[test]
fn empty_definition_is_a_valid_persistable_open_sketch() {
    let sketch = SketchDefinition::new();
    sketch
        .validate(PrecisionPolicy::default())
        .expect("empty sketch valid");
    let json = serde_json::to_string(&sketch).expect("serialize");
    let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded.revision(), SketchRevision::INITIAL);
    assert_eq!(decoded, sketch);
}

#[test]
fn legacy_profile_import_preserves_exact_analytic_curve_kinds_and_order() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(2.0, 0.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(1.0, 1.0),
                        start: Point2::new(2.0, 0.0),
                        end: Point2::new(0.0, 0.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                ],
            },
            holes: vec![PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(1.0, 0.5),
                    radius: 0.25,
                    direction: ArcDirection::Clockwise,
                }],
            }],
        }],
    };
    let sketch = SketchDefinition::from_legacy_profile(&profile, PrecisionPolicy::default())
        .expect("import legacy exact profile");
    assert_eq!(sketch.operations().len(), 1);
    assert!(matches!(
        sketch.operations()[0].recipe,
        SketchRecipe::LegacyImportedProfile { .. }
    ));
    let outputs = &sketch.operations()[0].outputs;
    let first = match outputs[&OutputRole::Curve(CurveOutputRole::ImportedCurve(0))] {
        artificer_sketch::SketchOutputRef::Curve(id) => sketch.entity(id).expect("first"),
        artificer_sketch::SketchOutputRef::Point(_) => panic!("curve"),
    };
    let second = match outputs[&OutputRole::Curve(CurveOutputRole::ImportedCurve(1))] {
        artificer_sketch::SketchOutputRef::Curve(id) => sketch.entity(id).expect("second"),
        artificer_sketch::SketchOutputRef::Point(_) => panic!("curve"),
    };
    let third = match outputs[&OutputRole::Curve(CurveOutputRole::ImportedCurve(2))] {
        artificer_sketch::SketchOutputRef::Curve(id) => sketch.entity(id).expect("third"),
        artificer_sketch::SketchOutputRef::Point(_) => panic!("curve"),
    };
    assert!(matches!(first.geometry, SketchCurve2::Line { .. }));
    assert!(matches!(second.geometry, SketchCurve2::CircularArc { .. }));
    assert!(matches!(third.geometry, SketchCurve2::Circle { .. }));
    assert_eq!(sketch.active_entities().count(), 3);
    sketch
        .validate(PrecisionPolicy::default())
        .expect("imported graph validates");
}

#[test]
fn legacy_import_reuses_identical_boundary_points_for_exact_connectivity() {
    let loop_points = [
        Point2::new(0.0, 0.0),
        Point2::new(2.0, 0.0),
        Point2::new(0.0, 2.0),
    ];
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&loop_points),
            holes: Vec::new(),
        }],
    };
    let sketch = SketchDefinition::from_legacy_profile(&profile, PrecisionPolicy::default())
        .expect("import triangle");
    assert_eq!(sketch.active_points().count(), 3);
    let curves = sketch
        .active_entities()
        .map(|entity| entity.geometry)
        .collect::<Vec<_>>();
    let SketchCurve2::Line { start: first, .. } = curves[0] else {
        panic!("line")
    };
    let SketchCurve2::Line { end: last, .. } = curves[2] else {
        panic!("line")
    };
    assert_eq!(first, last);
}

/// Relations are part of the sketch, not a UI overlay: they must survive the
/// document round trip, and the geometry must still follow them afterwards
/// (ADR 0026, F1).
#[test]
fn constraints_round_trip_with_the_definition() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch.stage(line(), "Line").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit");
    let (start, end) = {
        let mut points = sketch.points().keys().copied();
        (
            points.next().expect("start point"),
            points.next().expect("end point"),
        )
    };
    let relation = sketch
        .stage_constraint(
            artificer_sketch::SketchConstraintKind::Horizontal {
                first: start,
                second: end,
            },
            "Horizontal",
            PrecisionPolicy::default(),
        )
        .expect("stage the relation");
    sketch
        .commit(relation, ConfirmationSource::GreenTick)
        .expect("commit the relation");

    let json = serde_json::to_string_pretty(&sketch).expect("serialize");
    let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, sketch);
    assert_eq!(decoded.constraints().len(), 1);
    let solved = decoded
        .solve_constraints(PrecisionPolicy::default())
        .expect("the decoded sketch still solves");
    let first = solved.positions[&start];
    let second = solved.positions[&end];
    assert!(
        (first.v - second.v).abs() <= 1.0e-9,
        "the decoded relation must still hold the line level"
    );
}

use std::f64::consts::{PI, TAU};

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    Angle, CircularPatternDistribution, ConfirmationSource, CurveDirection, CurveOutputRole,
    EvaluatedCurve2, Integer, Length, OutputRole, PointInput, RetirementPolicy, SignedLength,
    SketchDefinition, SketchEntityId, SketchInputValues, SketchOutputRef, SketchPoint2,
    SketchRecipe, SketchTransactionError, SketchValidationError, SketchValue,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn length(value: f64) -> SketchValue<Length> {
    SketchValue::Literal(Length::new(value).expect("positive length"))
}

fn signed(value: f64) -> SketchValue<SignedLength> {
    SketchValue::Literal(SignedLength::new(value).expect("finite length"))
}

fn angle(value: f64) -> SketchValue<Angle> {
    SketchValue::Literal(Angle::radians(value).expect("finite angle"))
}

fn count(value: u16) -> SketchValue<Integer> {
    SketchValue::Literal(Integer::new(value))
}

fn line(start: (f64, f64), end: (f64, f64)) -> SketchRecipe {
    SketchRecipe::Line {
        start: point(start.0, start.1),
        end: point(end.0, end.1),
    }
}

fn commit(sketch: &mut SketchDefinition, recipe: SketchRecipe, label: &str) {
    let transaction = sketch.stage(recipe, label).expect("stage operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit operation");
}

fn operation_curve(sketch: &SketchDefinition, operation: usize) -> SketchEntityId {
    sketch.operations()[operation]
        .outputs
        .values()
        .find_map(|output| match output {
            SketchOutputRef::Curve(entity) => Some(*entity),
            SketchOutputRef::Point(_) => None,
        })
        .expect("operation curve output")
}

fn assert_point(point: SketchPoint2, expected: (f64, f64)) {
    assert!((point.u - expected.0).abs() < 1.0e-10, "u={}", point.u);
    assert!((point.v - expected.1).abs() < 1.0e-10, "v={}", point.v);
}

#[test]
fn rectangular_pattern_is_row_major_and_excludes_the_seed_instance() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((0.0, 0.0), (2.0, 0.0)), "Seed");
    let source = operation_curve(&sketch, 0);
    let recipe = SketchRecipe::RectangularPattern {
        sources: vec![source],
        columns: count(3),
        rows: count(2),
        column_spacing: signed(10.0),
        row_spacing: signed(5.0),
        direction: angle(0.0),
    };
    let transaction = sketch
        .stage(recipe, "Rectangular pattern")
        .expect("pattern");
    assert_eq!(transaction.impact().inserted_entities.len(), 5);
    assert_eq!(transaction.preview().active_entities().count(), 6);
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit pattern");

    let patterned =
        match sketch.operations()[1].outputs[&OutputRole::Curve(CurveOutputRole::PatternCurve {
            instance: 4,
            source: 0,
        })] {
            SketchOutputRef::Curve(entity) => entity,
            SketchOutputRef::Point(_) => panic!("curve output"),
        };
    let EvaluatedCurve2::Line { start, end } = sketch.evaluated_curve(patterned).expect("curve")
    else {
        panic!("line copy")
    };
    assert_point(start, (10.0, 5.0));
    assert_point(end, (12.0, 5.0));
}

#[test]
fn rectangular_pattern_selection_order_is_canonical_by_stable_id() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((0.0, 0.0), (1.0, 0.0)), "First");
    commit(&mut sketch, line((0.0, 2.0), (1.0, 2.0)), "Second");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    let build = |sources| SketchRecipe::RectangularPattern {
        sources,
        columns: count(2),
        rows: count(1),
        column_spacing: signed(5.0),
        row_spacing: signed(0.0),
        direction: angle(0.0),
    };
    let forward = sketch
        .stage(build(vec![first, second]), "Forward")
        .expect("stage");
    let reversed = sketch
        .stage(build(vec![second, first]), "Reversed")
        .expect("stage");
    let forward_curves = forward
        .preview()
        .active_entities()
        .skip(2)
        .map(|entity| entity.geometry.clone())
        .collect::<Vec<_>>();
    let reversed_curves = reversed
        .preview()
        .active_entities()
        .skip(2)
        .map(|entity| entity.geometry.clone())
        .collect::<Vec<_>>();
    assert_eq!(forward_curves, reversed_curves);
}

#[test]
fn circular_pattern_supports_rotated_and_orientation_preserving_instances() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((2.0, 0.0), (3.0, 0.0)), "Seed");
    let source = operation_curve(&sketch, 0);
    let rotated = SketchRecipe::CircularPattern {
        sources: vec![source],
        center: point(0.0, 0.0),
        count: count(4),
        total_angle: angle(TAU),
        distribution: CircularPatternDistribution::Complete,
        rotate_instances: true,
    };
    let transaction = sketch.stage(rotated, "Circular pattern").expect("pattern");
    let entity = match transaction.preview().operations()[1].outputs[&OutputRole::Curve(
        CurveOutputRole::PatternCurve {
            instance: 1,
            source: 0,
        },
    )] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    let EvaluatedCurve2::Line { start, end } = transaction
        .preview()
        .evaluated_curve(entity)
        .expect("rotated line")
    else {
        panic!("line")
    };
    assert_point(start, (0.0, 2.0));
    assert_point(end, (0.0, 3.0));

    let fixed = SketchRecipe::CircularPattern {
        sources: vec![source],
        center: point(0.0, 0.0),
        count: count(3),
        total_angle: angle(PI),
        distribution: CircularPatternDistribution::Extent,
        rotate_instances: false,
    };
    let transaction = sketch.stage(fixed, "Fixed orientation").expect("pattern");
    let entity = match transaction.preview().operations()[1].outputs[&OutputRole::Curve(
        CurveOutputRole::PatternCurve {
            instance: 2,
            source: 0,
        },
    )] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    let EvaluatedCurve2::Line { start, end } = transaction
        .preview()
        .evaluated_curve(entity)
        .expect("translated line")
    else {
        panic!("line")
    };
    assert_point(start, (-3.0, 0.0));
    assert_point(end, (-2.0, 0.0));
}

#[test]
fn pattern_recipe_replays_after_seed_edit_and_retains_output_ids() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((0.0, 0.0), (2.0, 0.0)), "Seed");
    let source = operation_curve(&sketch, 0);
    commit(
        &mut sketch,
        SketchRecipe::RectangularPattern {
            sources: vec![source],
            columns: count(2),
            rows: count(1),
            column_spacing: signed(10.0),
            row_spacing: signed(0.0),
            direction: angle(0.0),
        },
        "Pattern",
    );
    let pattern_outputs = sketch.operations()[1].outputs.clone();
    let replacement = sketch
        .stage_replace(
            sketch.operations()[0].id,
            line((0.0, 0.0), (4.0, 0.0)),
            "Resize seed",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("replay dependent pattern");
    assert_eq!(
        replacement.preview().operations()[1].outputs,
        pattern_outputs
    );
    let patterned = match pattern_outputs[&OutputRole::Curve(CurveOutputRole::PatternCurve {
        instance: 1,
        source: 0,
    })] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    let EvaluatedCurve2::Line { end, .. } = replacement
        .preview()
        .evaluated_curve(patterned)
        .expect("replayed copy")
    else {
        panic!("line")
    };
    assert_point(end, (14.0, 0.0));
}

#[test]
fn fillet_atomically_supersedes_sources_and_creates_exact_tangent_arc() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((-10.0, 0.0), (0.0, 0.0)), "Horizontal");
    commit(&mut sketch, line((0.0, 0.0), (0.0, 10.0)), "Vertical");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    let original = sketch.clone();
    let recipe = SketchRecipe::Fillet {
        first,
        second,
        radius: length(2.0),
    };
    let cancelled = sketch
        .stage_modifier(recipe.clone(), "Fillet")
        .expect("stage fillet")
        .cancel();
    assert_eq!(cancelled.unchanged_revision, sketch.revision());
    assert_eq!(sketch, original);

    let transaction = sketch.stage_modifier(recipe, "Fillet").expect("fillet");
    assert_eq!(transaction.preview().active_entities().count(), 3);
    assert_eq!(transaction.impact().superseded_entities.len(), 2);
    let modifier = transaction.preview().operations()[2].id;
    assert_eq!(
        transaction
            .preview()
            .entity(first)
            .expect("tombstone")
            .superseded_by,
        Some(modifier)
    );
    assert!(
        !transaction
            .preview()
            .entity(first)
            .expect("tombstone")
            .active
    );
    let arc = match transaction.preview().operations()[2].outputs
        [&OutputRole::Curve(CurveOutputRole::CornerConnector)]
    {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    let EvaluatedCurve2::CircularArc {
        center, start, end, ..
    } = transaction
        .preview()
        .evaluated_curve(arc)
        .expect("fillet arc")
    else {
        panic!("exact circular arc")
    };
    assert_point(center, (-2.0, 2.0));
    assert_point(start, (-2.0, 0.0));
    assert_point(end, (0.0, 2.0));
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit fillet");
    sketch.validate(PrecisionPolicy::default()).expect("replay");

    let encoded = serde_json::to_string(&sketch).expect("serialize");
    let decoded: SketchDefinition = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, sketch);
    decoded
        .validate(PrecisionPolicy::default())
        .expect("persisted replay");
}

#[test]
fn retiring_a_modifier_restores_its_source_branches() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((-10.0, 0.0), (0.0, 0.0)), "Horizontal");
    commit(&mut sketch, line((0.0, 0.0), (0.0, 10.0)), "Vertical");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    commit(
        &mut sketch,
        SketchRecipe::Fillet {
            first,
            second,
            radius: length(2.0),
        },
        "Fillet",
    );
    let modifier = sketch.operations()[2].id;
    let transaction = sketch
        .stage_retire_operation(
            modifier,
            RetirementPolicy::RejectDependents,
            "Delete fillet",
            PrecisionPolicy::default(),
        )
        .expect("retire modifier");
    assert_eq!(transaction.preview().active_entities().count(), 2);
    assert_eq!(transaction.impact().restored_entities.len(), 2);
    assert!(transaction.preview().entity(first).expect("source").active);
    assert!(transaction.preview().entity(second).expect("source").active);
}

#[test]
fn chamfer_uses_independent_edge_distances_and_stable_semantic_outputs() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((-10.0, 0.0), (0.0, 0.0)), "Horizontal");
    commit(&mut sketch, line((0.0, 0.0), (0.0, 10.0)), "Vertical");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    commit(
        &mut sketch,
        SketchRecipe::Chamfer {
            first,
            second,
            first_distance: length(2.0),
            second_distance: length(3.0),
        },
        "Chamfer",
    );
    let modifier = sketch.operations()[2].id;
    let original_outputs = sketch.operations()[2].outputs.clone();
    let connector = match original_outputs[&OutputRole::Curve(CurveOutputRole::CornerConnector)] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    let EvaluatedCurve2::Line { start, end } = sketch
        .evaluated_curve(connector)
        .expect("chamfer connector")
    else {
        panic!("line connector")
    };
    assert_point(start, (-2.0, 0.0));
    assert_point(end, (0.0, 3.0));

    let replacement = sketch
        .stage_replace(
            modifier,
            SketchRecipe::Chamfer {
                first,
                second,
                first_distance: length(4.0),
                second_distance: length(1.0),
            },
            "Edit chamfer",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("replace chamfer");
    assert_eq!(
        replacement.preview().operations()[2].outputs,
        original_outputs
    );
    assert_eq!(replacement.preview().active_entities().count(), 3);
}

#[test]
fn source_retirement_detects_entity_dependencies_and_can_cascade() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((-10.0, 0.0), (0.0, 0.0)), "Horizontal");
    commit(&mut sketch, line((0.0, 0.0), (0.0, 10.0)), "Vertical");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    commit(
        &mut sketch,
        SketchRecipe::Fillet {
            first,
            second,
            radius: length(2.0),
        },
        "Fillet",
    );
    let source_operation = sketch.operations()[0].id;
    assert!(matches!(
        sketch.stage_retire_operation(
            source_operation,
            RetirementPolicy::RejectDependents,
            "Reject",
            PrecisionPolicy::default(),
        ),
        Err(SketchTransactionError::DependentOperations { .. })
    ));
    let cascade = sketch
        .stage_retire_operation(
            source_operation,
            RetirementPolicy::CascadeDependents,
            "Cascade",
            PrecisionPolicy::default(),
        )
        .expect("cascade modifier");
    assert_eq!(cascade.impact().retired_operations.len(), 2);
    assert_eq!(cascade.preview().active_entities().count(), 1);
}

#[test]
fn invalid_pattern_and_corner_inputs_are_rejected_before_allocation() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((0.0, 0.0), (10.0, 0.0)), "First");
    commit(&mut sketch, line((0.0, 0.0), (0.0, 10.0)), "Second");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    let before = sketch.clone();

    let duplicate = SketchRecipe::RectangularPattern {
        sources: vec![first, first],
        columns: count(2),
        rows: count(1),
        column_spacing: signed(5.0),
        row_spacing: signed(0.0),
        direction: angle(0.0),
    };
    assert!(matches!(
        sketch.stage(duplicate, "Duplicate"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::DuplicateEntitySelection { .. }
        ))
    ));

    let oversized = SketchRecipe::Chamfer {
        first,
        second,
        first_distance: length(10.0),
        second_distance: length(2.0),
    };
    assert!(matches!(
        sketch.stage_modifier(oversized, "Oversized"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::CornerDistanceTooLarge
        ))
    ));
    assert_eq!(sketch, before);
    assert!(matches!(
        sketch.stage_modifier(line((0.0, 0.0), (1.0, 0.0)), "Not modifier"),
        Err(SketchTransactionError::NotAModifier)
    ));
}

#[test]
fn tampered_supersession_links_cannot_hide_or_reactivate_profile_geometry() {
    let mut sketch = SketchDefinition::new();
    commit(&mut sketch, line((-10.0, 0.0), (0.0, 0.0)), "Horizontal");
    commit(&mut sketch, line((0.0, 0.0), (0.0, 10.0)), "Vertical");
    let first = operation_curve(&sketch, 0);
    let second = operation_curve(&sketch, 1);
    commit(
        &mut sketch,
        SketchRecipe::Fillet {
            first,
            second,
            radius: length(2.0),
        },
        "Fillet",
    );

    let mut hidden_without_provenance = serde_json::to_value(&sketch).expect("encode");
    let entities = hidden_without_provenance["entities"]
        .as_object_mut()
        .expect("entity map");
    let first_record = entities
        .get_mut(&first.get().to_string())
        .expect("source record");
    first_record["superseded_by"] = serde_json::Value::Null;
    let tampered: SketchDefinition =
        serde_json::from_value(hidden_without_provenance).expect("decode structural graph");
    assert!(matches!(
        tampered.validate(PrecisionPolicy::default()),
        Err(SketchValidationError::EvaluatedCacheMismatch { .. })
    ));

    let mut reactivated = serde_json::to_value(&sketch).expect("encode");
    let entities = reactivated["entities"].as_object_mut().expect("entity map");
    entities
        .get_mut(&first.get().to_string())
        .expect("source record")["active"] = serde_json::json!(true);
    let tampered: SketchDefinition =
        serde_json::from_value(reactivated).expect("decode structural graph");
    assert!(matches!(
        tampered.validate(PrecisionPolicy::default()),
        Err(SketchValidationError::ActiveSupersededEntity { entity }) if entity == first
    ));
}

#[test]
fn compound_rectangle_can_be_filleted_and_dimension_replay_keeps_all_ids() {
    let mut sketch = SketchDefinition::new();
    commit(
        &mut sketch,
        SketchRecipe::TwoPointRectangle {
            first_corner: point(0.0, 0.0),
            width: signed(10.0),
            height: signed(10.0),
        },
        "Rectangle",
    );
    let rectangle = sketch.operations()[0].id;
    let first = match sketch.operations()[0].outputs[&OutputRole::Curve(CurveOutputRole::Side(0))] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("side curve"),
    };
    let second = match sketch.operations()[0].outputs[&OutputRole::Curve(CurveOutputRole::Side(1))]
    {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("side curve"),
    };
    commit(
        &mut sketch,
        SketchRecipe::Fillet {
            first,
            second,
            radius: length(1.0),
        },
        "Fillet rectangle",
    );
    let rectangle_outputs = sketch.operations()[0].outputs.clone();
    let fillet_outputs = sketch.operations()[1].outputs.clone();
    let replacement = sketch
        .stage_replace(
            rectangle,
            SketchRecipe::TwoPointRectangle {
                first_corner: point(0.0, 0.0),
                width: signed(20.0),
                height: signed(10.0),
            },
            "Resize rectangle",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("replay fillet after rectangle resize");
    assert_eq!(
        replacement.preview().operations()[0].outputs,
        rectangle_outputs
    );
    assert_eq!(
        replacement.preview().operations()[1].outputs,
        fillet_outputs
    );
    assert_eq!(replacement.preview().active_entities().count(), 5);
    replacement
        .preview()
        .validate(PrecisionPolicy::default())
        .expect("replacement graph replays");
}

#[test]
fn patterns_preserve_exact_circle_and_arc_carriers() {
    let mut sketch = SketchDefinition::new();
    commit(
        &mut sketch,
        SketchRecipe::CentrePointCircle {
            center: point(2.0, 0.0),
            radius: length(1.5),
            radial_angle: angle(0.0),
        },
        "Circle",
    );
    commit(
        &mut sketch,
        SketchRecipe::CentreStartEndArc {
            center: point(0.0, 0.0),
            start: point(1.0, 0.0),
            end: point(0.0, 1.0),
            direction: CurveDirection::CounterClockwise,
        },
        "Arc",
    );
    let circle = operation_curve(&sketch, 0);
    let arc = operation_curve(&sketch, 1);
    let transaction = sketch
        .stage(
            SketchRecipe::RectangularPattern {
                sources: vec![arc, circle],
                columns: count(2),
                rows: count(1),
                column_spacing: signed(5.0),
                row_spacing: signed(0.0),
                direction: angle(0.0),
            },
            "Pattern analytics",
        )
        .expect("pattern analytic curves");
    let circle_copy = match transaction.preview().operations()[2].outputs[&OutputRole::Curve(
        CurveOutputRole::PatternCurve {
            instance: 1,
            source: 0,
        },
    )] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    let arc_copy = match transaction.preview().operations()[2].outputs[&OutputRole::Curve(
        CurveOutputRole::PatternCurve {
            instance: 1,
            source: 1,
        },
    )] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve output"),
    };
    assert!(matches!(
        transaction.preview().evaluated_curve(circle_copy),
        Ok(EvaluatedCurve2::Circle { radius, .. }) if (radius - 1.5).abs() < 1.0e-12
    ));
    assert!(matches!(
        transaction.preview().evaluated_curve(arc_copy),
        Ok(EvaluatedCurve2::CircularArc {
            direction: CurveDirection::CounterClockwise,
            ..
        })
    ));
}

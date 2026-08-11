use std::collections::BTreeSet;
use std::f64::consts::TAU;
use std::time::Instant;

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    Angle, ArrangementDiagnostic, ArrangementInputCurve, ArrangementLimits,
    CircularPatternDistribution, ConfirmationSource, CurveOutputRole, Integer,
    MAX_ACTIVE_SKETCH_CURVES, MAX_PATTERN_INSTANCES, OutputRole, PointInput, PointOutputRole,
    SignedLength, SketchDefinition, SketchEntityId, SketchInputValues, SketchOutputRef,
    SketchPoint2, SketchPointId, SketchRecipe, SketchTransactionError, SketchValidationError,
    SketchValue, build_arrangement, evaluate_recipe,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn signed(value: f64) -> SketchValue<SignedLength> {
    SketchValue::Literal(SignedLength::new(value).expect("finite signed length"))
}

fn angle(value: f64) -> SketchValue<Angle> {
    SketchValue::Literal(Angle::radians(value).expect("finite angle"))
}

fn count(value: u16) -> SketchValue<Integer> {
    SketchValue::Literal(Integer::new(value))
}

fn commit(sketch: &mut SketchDefinition, recipe: SketchRecipe, label: &str) {
    let transaction = sketch.stage(recipe, label).expect("stage operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit operation");
}

fn seed_line(sketch: &mut SketchDefinition, start: (f64, f64), end: (f64, f64)) -> SketchEntityId {
    commit(
        sketch,
        SketchRecipe::Line {
            start: point(start.0, start.1),
            end: point(end.0, end.1),
        },
        "Seed line",
    );
    sketch.active_entities().last().expect("seed entity").id
}

fn rectangular_pattern(
    source: SketchEntityId,
    columns: u16,
    rows: u16,
    column_spacing: f64,
    row_spacing: f64,
) -> SketchRecipe {
    SketchRecipe::RectangularPattern {
        sources: vec![source],
        columns: count(columns),
        rows: count(rows),
        column_spacing: signed(column_spacing),
        row_spacing: signed(row_spacing),
        direction: angle(0.0),
    }
}

fn circular_pattern(source: SketchEntityId, count_value: u16) -> SketchRecipe {
    SketchRecipe::CircularPattern {
        sources: vec![source],
        center: point(0.0, 0.0),
        count: count(count_value),
        total_angle: angle(TAU),
        distribution: CircularPatternDistribution::Complete,
        rotate_instances: true,
    }
}

fn output_id(
    sketch: &SketchDefinition,
    operation_index: usize,
    role: OutputRole,
) -> SketchOutputRef {
    sketch.operations()[operation_index].outputs[&role]
}

#[test]
fn pattern_cardinality_is_exact_across_deterministic_boundary_cases() {
    let mut sketch = SketchDefinition::new();
    let source = seed_line(&mut sketch, (2.0, 0.0), (3.0, 0.0));
    let precision = PrecisionPolicy::default();
    let inputs = SketchInputValues::default();

    for (columns, rows) in [(2, 1), (3, 4), (16, 16), (256, 1)] {
        let expected = usize::from(columns) * usize::from(rows) - 1;
        let evaluation = evaluate_recipe(
            &sketch,
            &rectangular_pattern(source, columns, rows, 4.0, 7.0),
            &inputs,
            precision,
        )
        .expect("rectangular pattern");
        assert_eq!(evaluation.curves.len(), expected);
        assert_eq!(evaluation.points.len(), expected * 2);
        assert_eq!(
            evaluation
                .curves
                .iter()
                .map(|curve| curve.role)
                .collect::<BTreeSet<_>>()
                .len(),
            expected
        );
    }

    for count_value in [2, 3, 16, MAX_PATTERN_INSTANCES] {
        let expected = usize::from(count_value - 1);
        let evaluation = evaluate_recipe(
            &sketch,
            &circular_pattern(source, count_value),
            &inputs,
            precision,
        )
        .expect("circular pattern");
        assert_eq!(evaluation.curves.len(), expected);
        assert_eq!(evaluation.points.len(), expected * 2 + 1);
    }
}

#[test]
fn count_edits_preserve_surviving_semantic_ids_and_never_reuse_retired_ids() {
    let mut sketch = SketchDefinition::new();
    let source = seed_line(&mut sketch, (0.0, 0.0), (1.0, 0.0));
    commit(
        &mut sketch,
        rectangular_pattern(source, 4, 1, 5.0, 0.0),
        "Pattern",
    );
    let operation = sketch.operations()[1].id;
    let surviving_curve_role = OutputRole::Curve(CurveOutputRole::PatternCurve {
        instance: 1,
        source: 0,
    });
    let surviving_point_role = OutputRole::Point(PointOutputRole::PatternPoint {
        instance: 1,
        source: 0,
        point: 0,
    });
    let retired_curve_role = OutputRole::Curve(CurveOutputRole::PatternCurve {
        instance: 2,
        source: 0,
    });
    let surviving_curve = output_id(&sketch, 1, surviving_curve_role);
    let surviving_point = output_id(&sketch, 1, surviving_point_role);
    let SketchOutputRef::Curve(retired_curve) = output_id(&sketch, 1, retired_curve_role) else {
        panic!("pattern curve role")
    };
    let old_entity_high_water = sketch.high_water_marks().entity();

    let shrink = sketch
        .stage_replace(
            operation,
            rectangular_pattern(source, 2, 1, 5.0, 0.0),
            "Shrink pattern",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("shrink pattern");
    assert_eq!(
        output_id(shrink.preview(), 1, surviving_curve_role),
        surviving_curve
    );
    assert_eq!(
        output_id(shrink.preview(), 1, surviving_point_role),
        surviving_point
    );
    assert!(
        !shrink.preview().operations()[1]
            .outputs
            .contains_key(&retired_curve_role)
    );
    assert!(
        !shrink
            .preview()
            .entity(retired_curve)
            .expect("tombstone")
            .active
    );
    sketch
        .commit(shrink, ConfirmationSource::GreenTick)
        .expect("commit shrink");

    let expand = sketch
        .stage_replace(
            operation,
            rectangular_pattern(source, 5, 1, 5.0, 0.0),
            "Expand pattern",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("expand pattern");
    assert_eq!(
        output_id(expand.preview(), 1, surviving_curve_role),
        surviving_curve
    );
    assert_eq!(
        output_id(expand.preview(), 1, surviving_point_role),
        surviving_point
    );
    let SketchOutputRef::Curve(recreated_curve) =
        output_id(expand.preview(), 1, retired_curve_role)
    else {
        panic!("recreated pattern curve")
    };
    assert_ne!(recreated_curve, retired_curve);
    assert!(recreated_curve.get() > old_entity_high_water);
    assert!(
        !expand
            .preview()
            .entity(retired_curve)
            .expect("old tombstone")
            .active
    );
}

#[test]
fn maximum_pattern_fills_the_active_curve_budget_and_has_a_non_flaky_timing_smoke() {
    let mut sketch = SketchDefinition::new();
    commit(
        &mut sketch,
        SketchRecipe::TwoPointRectangle {
            first_corner: point(0.0, 0.0),
            width: signed(1.0),
            height: signed(1.0),
        },
        "Seed rectangle",
    );
    let sources = sketch
        .active_entities()
        .map(|entity| entity.id)
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 4);
    let recipe = SketchRecipe::RectangularPattern {
        sources,
        columns: count(MAX_PATTERN_INSTANCES),
        rows: count(1),
        column_spacing: signed(2.0),
        row_spacing: signed(0.0),
        direction: angle(0.0),
    };

    let started = Instant::now();
    let transaction = sketch
        .stage(recipe, "Maximum pattern")
        .expect("maximum pattern");
    let elapsed = started.elapsed();
    eprintln!("maximum 256-instance/1024-curve pattern staged in {elapsed:?}");
    assert_eq!(transaction.impact().inserted_entities.len(), 1_020);
    assert_eq!(
        transaction.preview().active_entities().count(),
        MAX_ACTIVE_SKETCH_CURVES
    );
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit maximum pattern");
    sketch
        .validate(PrecisionPolicy::default())
        .expect("maximum replay");

    assert!(matches!(
        sketch.stage(
            SketchRecipe::Line {
                start: point(0.0, 10.0),
                end: point(1.0, 10.0),
            },
            "One curve too many"
        ),
        Err(SketchTransactionError::Validation(
            SketchValidationError::ResourceLimit {
                resource: "active_curves",
                requested: 1_025,
                limit: MAX_ACTIVE_SKETCH_CURVES,
            }
        ))
    ));
}

#[test]
fn hostile_pattern_counts_products_spacing_and_coordinate_envelopes_fail_closed() {
    let mut sketch = SketchDefinition::new();
    let source = seed_line(&mut sketch, (-1.0, 0.0), (0.0, 0.0));
    let inputs = SketchInputValues::default();
    let precision = PrecisionPolicy::default();

    for (recipe, expected_count, minimum) in [
        (rectangular_pattern(source, 0, 2, 1.0, 1.0), 0, 1),
        (rectangular_pattern(source, 257, 1, 1.0, 0.0), 257, 1),
        (circular_pattern(source, 0), 0, 2),
        (circular_pattern(source, 1), 1, 2),
        (circular_pattern(source, 257), 257, 2),
    ] {
        assert!(matches!(
            evaluate_recipe(&sketch, &recipe, &inputs, precision),
            Err(SketchValidationError::PatternCount { count, minimum: actual_minimum })
                if count == expected_count && actual_minimum == minimum
        ));
    }

    assert!(matches!(
        evaluate_recipe(
            &sketch,
            &rectangular_pattern(source, 1, 1, 0.0, 0.0),
            &inputs,
            precision,
        ),
        Err(SketchValidationError::ResourceLimit {
            resource: "pattern_instances",
            requested: 1,
            limit: 256,
        })
    ));
    assert!(matches!(
        evaluate_recipe(
            &sketch,
            &rectangular_pattern(source, 256, 2, 1.0, 1.0),
            &inputs,
            precision,
        ),
        Err(SketchValidationError::ResourceLimit {
            resource: "pattern_instances",
            requested: 512,
            limit: 256,
        })
    ));
    assert!(matches!(
        evaluate_recipe(
            &sketch,
            &rectangular_pattern(source, 2, 1, 0.0, 0.0),
            &inputs,
            precision,
        ),
        Err(SketchValidationError::FeatureTooSmall { .. })
    ));

    let exact_boundary_spacing = precision.max_abs_coordinate / 255.0;
    evaluate_recipe(
        &sketch,
        &rectangular_pattern(source, 256, 1, exact_boundary_spacing, 0.0),
        &inputs,
        precision,
    )
    .expect("coordinate exactly on the positive envelope");
    assert!(matches!(
        evaluate_recipe(
            &sketch,
            &rectangular_pattern(
                source,
                256,
                1,
                (precision.max_abs_coordinate + 1_024.0) / 255.0,
                0.0,
            ),
            &inputs,
            precision,
        ),
        Err(SketchValidationError::CoordinateOutOfBounds { .. })
    ));
}

fn arrangement_line(
    entity: u64,
    point_base: u64,
    start: SketchPoint2,
    end: SketchPoint2,
) -> ArrangementInputCurve {
    ArrangementInputCurve::line(
        SketchEntityId::new(entity).expect("entity ID"),
        SketchPointId::new(point_base).expect("start point ID"),
        SketchPointId::new(point_base + 1).expect("end point ID"),
        start,
        end,
    )
}

fn crossing_grid(vertical_count: usize, horizontal_count: usize) -> Vec<ArrangementInputCurve> {
    let mut curves = Vec::with_capacity(vertical_count + horizontal_count);
    for index in 0..vertical_count {
        curves.push(arrangement_line(
            curves.len() as u64 + 1,
            curves.len() as u64 * 2 + 1,
            SketchPoint2::new(index as f64, -1.0),
            SketchPoint2::new(index as f64, horizontal_count as f64),
        ));
    }
    for index in 0..horizontal_count {
        curves.push(arrangement_line(
            curves.len() as u64 + 1,
            curves.len() as u64 * 2 + 1,
            SketchPoint2::new(-1.0, index as f64),
            SketchPoint2::new(vertical_count as f64, index as f64),
        ));
    }
    curves
}

#[test]
fn arrangement_accepts_exact_curve_and_event_ceilings_and_rejects_the_next_value() {
    let precision = PrecisionPolicy::default();
    let exactly_max_curves = (0..1_024)
        .map(|index| {
            arrangement_line(
                index as u64 + 1,
                index as u64 * 2 + 1,
                SketchPoint2::new(0.0, index as f64 * 2.0),
                SketchPoint2::new(1.0, index as f64 * 2.0),
            )
        })
        .collect::<Vec<_>>();
    let exact = build_arrangement(
        &exactly_max_curves,
        &precision,
        ArrangementLimits::default(),
    );
    assert!(
        !exact.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            ArrangementDiagnostic::CurveLimitExceeded { .. }
        ))
    );

    let mut one_too_many = exactly_max_curves;
    one_too_many.push(arrangement_line(
        1_025,
        2_049,
        SketchPoint2::new(0.0, 2_048.0),
        SketchPoint2::new(1.0, 2_048.0),
    ));
    let rejected = build_arrangement(&one_too_many, &precision, ArrangementLimits::default());
    assert_eq!(
        rejected.diagnostics,
        vec![ArrangementDiagnostic::CurveLimitExceeded {
            limit: 1_024,
            actual: 1_025,
        }]
    );

    let event_limits = ArrangementLimits {
        max_curves: 1_024,
        max_intersection_events: 1_024,
        max_fragments: ArrangementLimits::default().max_fragments,
    };
    let exact_grid = build_arrangement(&crossing_grid(32, 32), &precision, event_limits);
    assert!(
        !exact_grid.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            ArrangementDiagnostic::EventLimitExceeded { .. }
        ))
    );
    assert_eq!(exact_grid.cells.len(), 31 * 31);

    let rejected_grid = build_arrangement(&crossing_grid(33, 32), &precision, event_limits);
    assert!(rejected_grid.diagnostics.iter().any(|diagnostic| matches!(
        diagnostic,
        ArrangementDiagnostic::EventLimitExceeded { limit: 1_024 }
    )));
    assert!(rejected_grid.cells.is_empty());
}

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    Angle, ConfirmationSource, CurveDirection, CurveOutputRole, EvaluatedCurve2, FilletBranchHints,
    Length, OutputRole, PointInput, SketchDefinition, SketchEntityId, SketchInputValues,
    SketchOutputRef, SketchPoint2, SketchRecipe, SketchTransactionError, SketchValidationError,
    SketchValue,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn length(value: f64) -> SketchValue<Length> {
    SketchValue::Literal(Length::new(value).expect("positive length"))
}

fn commit_curve(sketch: &mut SketchDefinition, recipe: SketchRecipe) -> SketchEntityId {
    let transaction = sketch.stage(recipe, "Source").expect("stage source");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit source");
    sketch
        .operations()
        .last()
        .expect("source operation")
        .outputs
        .values()
        .find_map(|output| match output {
            SketchOutputRef::Curve(entity) => Some(*entity),
            SketchOutputRef::Point(_) => None,
        })
        .expect("source curve")
}

fn line(sketch: &mut SketchDefinition, start: (f64, f64), end: (f64, f64)) -> SketchEntityId {
    commit_curve(
        sketch,
        SketchRecipe::Line {
            start: point(start.0, start.1),
            end: point(end.0, end.1),
        },
    )
}

fn circle(sketch: &mut SketchDefinition, center: (f64, f64), radius: f64) -> SketchEntityId {
    commit_curve(
        sketch,
        SketchRecipe::CentrePointCircle {
            center: point(center.0, center.1),
            radius: length(radius),
            radial_angle: SketchValue::Literal(Angle::radians(0.0).expect("finite angle")),
        },
    )
}

fn arc(
    sketch: &mut SketchDefinition,
    center: (f64, f64),
    start: (f64, f64),
    end: (f64, f64),
) -> SketchEntityId {
    commit_curve(
        sketch,
        SketchRecipe::CentreStartEndArc {
            center: point(center.0, center.1),
            start: point(start.0, start.1),
            end: point(end.0, end.1),
            direction: CurveDirection::CounterClockwise,
        },
    )
}

#[derive(Clone)]
struct PairFixture {
    name: &'static str,
    sketch: SketchDefinition,
    first: SketchEntityId,
    second: SketchEntityId,
    radius: f64,
    hints: FilletBranchHints,
}

fn line_line_fixture() -> PairFixture {
    let mut sketch = SketchDefinition::new();
    let first = line(&mut sketch, (-10.0, 0.0), (0.0, 0.0));
    let second = line(&mut sketch, (0.0, 0.0), (0.0, 10.0));
    PairFixture {
        name: "line-line",
        sketch,
        first,
        second,
        radius: 2.0,
        hints: FilletBranchHints {
            first_pick: SketchPoint2::new(-8.0, 0.0),
            second_pick: SketchPoint2::new(0.0, 8.0),
            corner_hint: SketchPoint2::new(0.0, 0.0),
        },
    }
}

fn line_circle_fixture(circle_is_arc: bool) -> PairFixture {
    let mut sketch = SketchDefinition::new();
    let first = line(&mut sketch, (0.0, 0.0), (12.0, 0.0));
    let second = if circle_is_arc {
        arc(&mut sketch, (5.0, 3.0), (5.0, -2.0), (5.0, 8.0))
    } else {
        circle(&mut sketch, (5.0, 3.0), 5.0)
    };
    PairFixture {
        name: if circle_is_arc {
            "line-arc"
        } else {
            "line-circle"
        },
        sketch,
        first,
        second,
        radius: 1.0,
        hints: FilletBranchHints {
            first_pick: SketchPoint2::new(11.0, 0.0),
            second_pick: SketchPoint2::new(5.0, 8.0),
            corner_hint: SketchPoint2::new(9.0, 0.0),
        },
    }
}

fn circular_pair_fixture(first_is_arc: bool, second_is_arc: bool) -> PairFixture {
    let mut sketch = SketchDefinition::new();
    let first = if first_is_arc {
        arc(&mut sketch, (0.0, 0.0), (5.0, 0.0), (3.0, 4.0))
    } else {
        circle(&mut sketch, (0.0, 0.0), 5.0)
    };
    let second = if second_is_arc {
        arc(&mut sketch, (6.0, 0.0), (3.0, 4.0), (1.0, 0.0))
    } else {
        circle(&mut sketch, (6.0, 0.0), 5.0)
    };
    let first_pick = if first_is_arc {
        SketchPoint2::new(5.0, 0.0)
    } else {
        SketchPoint2::new(0.0, -5.0)
    };
    let second_pick = SketchPoint2::new(1.0, 0.0);
    PairFixture {
        name: match (first_is_arc, second_is_arc) {
            (true, true) => "arc-arc",
            (true, false) => "arc-circle",
            (false, false) => "circle-circle",
            (false, true) => unreachable!("covered by unordered arc-circle"),
        },
        sketch,
        first,
        second,
        radius: 0.5,
        hints: FilletBranchHints {
            first_pick,
            second_pick,
            corner_hint: SketchPoint2::new(3.0, 4.0),
        },
    }
}

fn recipe(fixture: &PairFixture, reversed: bool) -> SketchRecipe {
    let (first, second, first_pick, second_pick) = if reversed {
        (
            fixture.second,
            fixture.first,
            fixture.hints.second_pick,
            fixture.hints.first_pick,
        )
    } else {
        (
            fixture.first,
            fixture.second,
            fixture.hints.first_pick,
            fixture.hints.second_pick,
        )
    };
    SketchRecipe::FilletWithHints {
        first,
        second,
        radius: length(fixture.radius),
        hints: FilletBranchHints {
            first_pick,
            second_pick,
            corner_hint: fixture.hints.corner_hint,
        },
    }
}

fn staged_connector(
    fixture: &PairFixture,
    reversed: bool,
) -> Result<(SketchDefinition, EvaluatedCurve2), SketchTransactionError> {
    let transaction = fixture
        .sketch
        .stage_modifier(recipe(fixture, reversed), "Fillet")?;
    let preview = transaction.preview().clone();
    let operation = preview.operations().last().expect("fillet operation");
    let connector = match operation.outputs[&OutputRole::Curve(CurveOutputRole::CornerConnector)] {
        SketchOutputRef::Curve(entity) => preview.evaluated_curve(entity).expect("connector"),
        SketchOutputRef::Point(_) => panic!("connector must be a curve"),
    };
    Ok((preview, connector))
}

fn operation_curve_by_role(
    sketch: &SketchDefinition,
    role: CurveOutputRole,
) -> (SketchEntityId, EvaluatedCurve2) {
    let operation = sketch.operations().last().expect("fillet operation");
    let entity = match operation.outputs[&OutputRole::Curve(role)] {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve role resolved to a point"),
    };
    (
        entity,
        sketch.evaluated_curve(entity).expect("evaluated output"),
    )
}

fn assert_retained_is_source_subset(retained: &EvaluatedCurve2, source: &EvaluatedCurve2) {
    for parameter in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let point = retained.evaluate(parameter).expect("finite retained use");
        let source_parameter = source.closest_parameter(point);
        let source_point = source
            .evaluate(if source.is_periodic() && source_parameter == 1.0 {
                0.0
            } else {
                source_parameter
            })
            .expect("finite source use");
        assert!(
            point.distance(source_point) < 1.0e-8,
            "retained geometry extended beyond its finite source"
        );
    }
}

fn assert_parallel(
    first: artificer_sketch::SketchVector2,
    second: artificer_sketch::SketchVector2,
) {
    let normalized_cross = first.cross(second).abs() / (first.length() * second.length());
    assert!(normalized_cross < 1.0e-6, "cross={normalized_cross}");
}

fn assert_exact_fillet_contract(fixture: &PairFixture, preview: &SketchDefinition) {
    let (_, first_retained) = operation_curve_by_role(preview, CurveOutputRole::TrimmedSource(0));
    let (_, second_retained) = operation_curve_by_role(preview, CurveOutputRole::TrimmedSource(1));
    let (_, connector) = operation_curve_by_role(preview, CurveOutputRole::CornerConnector);
    let first_source = fixture
        .sketch
        .evaluated_curve(fixture.first)
        .expect("first source");
    let second_source = fixture
        .sketch
        .evaluated_curve(fixture.second)
        .expect("second source");
    assert_retained_is_source_subset(&first_retained, &first_source);
    assert_retained_is_source_subset(&second_retained, &second_source);

    let EvaluatedCurve2::CircularArc {
        center, start, end, ..
    } = connector
    else {
        panic!("connector must remain analytic")
    };
    assert!((center.distance(start) - fixture.radius).abs() < 1.0e-8);
    assert!((center.distance(end) - fixture.radius).abs() < 1.0e-8);
    let first_parameter = first_source.closest_parameter(start);
    let second_parameter = second_source.closest_parameter(end);
    assert_parallel(
        first_source
            .tangent(first_parameter)
            .expect("first tangent"),
        connector.tangent(0.0).expect("connector start tangent"),
    );
    assert_parallel(
        second_source
            .tangent(second_parameter)
            .expect("second tangent"),
        connector.tangent(1.0).expect("connector end tangent"),
    );

    let modifier = preview.operations().last().expect("fillet operation").id;
    for source in [fixture.first, fixture.second] {
        let tombstone = preview.entity(source).expect("source tombstone");
        assert!(!tombstone.active);
        assert_eq!(tombstone.superseded_by, Some(modifier));
    }
}

fn assert_same_arc_use(first: EvaluatedCurve2, second: EvaluatedCurve2) {
    let EvaluatedCurve2::CircularArc {
        center: first_center,
        start: first_start,
        end: first_end,
        ..
    } = first
    else {
        panic!("first connector is not an arc")
    };
    let EvaluatedCurve2::CircularArc {
        center: second_center,
        start: second_start,
        end: second_end,
        ..
    } = second
    else {
        panic!("second connector is not an arc")
    };
    let near = |first: SketchPoint2, second: SketchPoint2| first.distance(second) < 1.0e-8;
    assert!(near(first_center, second_center));
    assert!(
        (near(first_start, second_start) && near(first_end, second_end))
            || (near(first_start, second_end) && near(first_end, second_start))
    );
}

#[test]
fn all_unordered_line_arc_circle_pairs_have_exact_no_extension_fillets() {
    let fixtures = [
        line_line_fixture(),
        line_circle_fixture(true),
        line_circle_fixture(false),
        circular_pair_fixture(true, true),
        circular_pair_fixture(true, false),
        circular_pair_fixture(false, false),
    ];
    for fixture in fixtures {
        let (preview, connector) = staged_connector(&fixture, false)
            .unwrap_or_else(|error| panic!("{} failed: {error}", fixture.name));
        assert_eq!(preview.active_entities().count(), 3, "{}", fixture.name);
        connector
            .validate(&PrecisionPolicy::default())
            .unwrap_or_else(|error| panic!("{} connector invalid: {error}", fixture.name));
        assert_exact_fillet_contract(&fixture, &preview);

        let (_, reversed) = staged_connector(&fixture, true)
            .unwrap_or_else(|error| panic!("{} reversed failed: {error}", fixture.name));
        assert_same_arc_use(connector, reversed);
    }
}

#[test]
fn hinted_fillet_round_trips_and_replays_with_stable_output_ids() {
    let fixture = circular_pair_fixture(true, false);
    let transaction = fixture
        .sketch
        .stage_modifier(recipe(&fixture, false), "Fillet")
        .expect("stage fillet");
    let mut sketch = fixture.sketch;
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit fillet");
    let operation = sketch.operations().last().expect("fillet operation").id;
    let outputs = sketch
        .operations()
        .last()
        .expect("fillet operation")
        .outputs
        .clone();
    let encoded = serde_json::to_string(&sketch).expect("serialize");
    let decoded: SketchDefinition = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, sketch);
    decoded
        .validate(PrecisionPolicy::default())
        .expect("replay decoded fillet");

    let replacement = sketch
        .stage_replace(
            operation,
            SketchRecipe::FilletWithHints {
                first: fixture.first,
                second: fixture.second,
                radius: length(0.4),
                hints: fixture.hints,
            },
            "Resize fillet",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("replace fillet");
    assert_eq!(
        replacement.preview().operations().last().unwrap().outputs,
        outputs
    );
}

#[test]
fn tangent_concentric_ambiguous_oversized_and_degenerate_inputs_fail_closed() {
    let mut tangent = SketchDefinition::new();
    let tangent_line = line(&mut tangent, (-10.0, 5.0), (10.0, 5.0));
    let tangent_circle = circle(&mut tangent, (0.0, 0.0), 5.0);
    let tangent_recipe = SketchRecipe::FilletWithHints {
        first: tangent_line,
        second: tangent_circle,
        radius: length(1.0),
        hints: FilletBranchHints {
            first_pick: SketchPoint2::new(5.0, 5.0),
            second_pick: SketchPoint2::new(5.0, 0.0),
            corner_hint: SketchPoint2::new(0.0, 5.0),
        },
    };
    assert!(matches!(
        tangent.stage_modifier(tangent_recipe, "Tangent"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::FilletNoBoundedSolution
        ))
    ));

    let mut concentric = SketchDefinition::new();
    let first_circle = circle(&mut concentric, (0.0, 0.0), 5.0);
    let second_circle = circle(&mut concentric, (0.0, 0.0), 3.0);
    let concentric_recipe = SketchRecipe::FilletWithHints {
        first: first_circle,
        second: second_circle,
        radius: length(0.5),
        hints: FilletBranchHints {
            first_pick: SketchPoint2::new(5.0, 0.0),
            second_pick: SketchPoint2::new(3.0, 0.0),
            corner_hint: SketchPoint2::new(4.0, 0.0),
        },
    };
    assert!(matches!(
        concentric.stage_modifier(concentric_recipe, "Concentric"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::FilletNoBoundedSolution
        ))
    ));

    let mut ambiguous = SketchDefinition::new();
    let first_circle = circle(&mut ambiguous, (0.0, 0.0), 5.0);
    let second_circle = circle(&mut ambiguous, (6.0, 0.0), 5.0);
    let ambiguous_recipe = SketchRecipe::FilletWithHints {
        first: first_circle,
        second: second_circle,
        radius: length(0.5),
        hints: FilletBranchHints {
            first_pick: SketchPoint2::new(-5.0, 0.0),
            second_pick: SketchPoint2::new(11.0, 0.0),
            corner_hint: SketchPoint2::new(3.0, 0.0),
        },
    };
    assert!(matches!(
        ambiguous.stage_modifier(ambiguous_recipe, "Ambiguous"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::FilletAmbiguousSolution
        ))
    ));

    let fixture = line_line_fixture();
    let oversized = SketchRecipe::FilletWithHints {
        first: fixture.first,
        second: fixture.second,
        radius: length(20.0),
        hints: fixture.hints,
    };
    assert!(matches!(
        fixture.sketch.stage_modifier(oversized, "Oversized"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::FilletNoBoundedSolution
        ))
    ));

    let near_degenerate = SketchRecipe::FilletWithHints {
        first: fixture.first,
        second: fixture.second,
        radius: length(PrecisionPolicy::default().min_feature_size),
        hints: fixture.hints,
    };
    assert!(matches!(
        fixture
            .sketch
            .stage_modifier(near_degenerate, "Near degenerate"),
        Err(SketchTransactionError::Validation(
            SketchValidationError::FilletNoBoundedSolution
                | SketchValidationError::FeatureTooSmall { .. }
        ))
    ));
}

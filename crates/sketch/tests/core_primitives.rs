use std::collections::BTreeSet;

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    Angle, ArrangementLimits, ConfirmationSource, CurveDraft2, CurveOutputRole, Integer, Length,
    PointInput, PointOutputRole, SignedLength, SketchDefinition, SketchEntityRole,
    SketchInputValues, SketchPoint2, SketchRecipe, SketchValidationError, SketchValue,
    build_arrangement, evaluate_recipe,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn length(value: f64) -> SketchValue<Length> {
    SketchValue::Literal(Length::new(value).expect("length"))
}

fn signed(value: f64) -> SketchValue<SignedLength> {
    SketchValue::Literal(SignedLength::new(value).expect("signed length"))
}

fn angle(value: f64) -> SketchValue<Angle> {
    SketchValue::Literal(Angle::radians(value).expect("angle"))
}

fn evaluate(recipe: &SketchRecipe) -> artificer_sketch::PrimitiveEvaluation {
    evaluate_recipe(
        &SketchDefinition::new(),
        recipe,
        &SketchInputValues::default(),
        PrecisionPolicy::default(),
    )
    .expect("valid primitive")
}

#[test]
fn all_creation_recipes_have_deterministic_atomic_output_counts() {
    let cases = [
        (
            SketchRecipe::Line {
                start: point(0.0, 0.0),
                end: point(5.0, 0.0),
            },
            2,
            1,
        ),
        (
            SketchRecipe::CentreLine {
                start: point(0.0, 0.0),
                end: point(5.0, 0.0),
            },
            2,
            1,
        ),
        (
            SketchRecipe::Polyline {
                vertices: vec![point(0.0, 0.0), point(5.0, 0.0), point(5.0, 5.0)],
                closed: true,
                construction: false,
            },
            3,
            3,
        ),
        (
            SketchRecipe::TwoPointRectangle {
                first_corner: point(0.0, 0.0),
                width: signed(8.0),
                height: signed(4.0),
            },
            5,
            4,
        ),
        (
            SketchRecipe::CentrePointRectangle {
                center: point(0.0, 0.0),
                width: length(8.0),
                height: length(4.0),
            },
            5,
            4,
        ),
        (
            SketchRecipe::CentrePointCircle {
                center: point(0.0, 0.0),
                radius: length(3.0),
                radial_angle: angle(0.0),
            },
            2,
            1,
        ),
        (
            SketchRecipe::TwoPointCircle {
                first_diameter_point: point(-3.0, 0.0),
                second_diameter_point: point(3.0, 0.0),
                direction: artificer_sketch::CurveDirection::CounterClockwise,
            },
            3,
            1,
        ),
        (
            SketchRecipe::CentreStartEndArc {
                center: point(0.0, 0.0),
                start: point(3.0, 0.0),
                end: point(0.0, 3.0),
                direction: artificer_sketch::CurveDirection::CounterClockwise,
            },
            3,
            1,
        ),
        (
            SketchRecipe::InnerDiameterPolygon {
                center: point(0.0, 0.0),
                inner_diameter: length(6.0),
                sides: SketchValue::Literal(Integer::new(5)),
                rotation: angle(0.0),
            },
            6,
            5,
        ),
        (
            SketchRecipe::OuterDiameterPolygon {
                center: point(0.0, 0.0),
                outer_diameter: length(6.0),
                sides: SketchValue::Literal(Integer::new(5)),
                rotation: angle(0.0),
            },
            6,
            5,
        ),
        (
            SketchRecipe::TwoPointSlot {
                first_cap_center: point(-4.0, 0.0),
                second_cap_center: point(4.0, 0.0),
                width: length(2.0),
            },
            7,
            4,
        ),
        (
            SketchRecipe::CentreOuterPointSlot {
                center: point(0.0, 0.0),
                overall_length: length(10.0),
                width: length(2.0),
                angle: angle(0.0),
            },
            7,
            4,
        ),
    ];

    for (recipe, point_count, curve_count) in cases {
        let first = evaluate(&recipe);
        let second = evaluate(&recipe);
        assert_eq!(first, second, "recipe must evaluate deterministically");
        assert_eq!(first.points.len(), point_count);
        assert_eq!(first.curves.len(), curve_count);
        assert_eq!(
            first
                .points
                .iter()
                .map(|point| point.role)
                .collect::<BTreeSet<_>>()
                .len(),
            point_count
        );
    }
}

#[test]
fn centreline_is_construction_and_never_profile_geometry() {
    let output = evaluate(&SketchRecipe::CentreLine {
        start: point(0.0, 0.0),
        end: point(10.0, 0.0),
    });
    assert_eq!(output.curves[0].entity_role, SketchEntityRole::Construction);
}

#[test]
fn slots_publish_stable_rail_and_cap_roles_with_analytic_arcs() {
    let output = evaluate(&SketchRecipe::TwoPointSlot {
        first_cap_center: point(-4.0, 0.0),
        second_cap_center: point(4.0, 0.0),
        width: length(2.0),
    });
    let roles = output
        .curves
        .iter()
        .map(|curve| curve.role)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        roles,
        BTreeSet::from([
            CurveOutputRole::Rail(0),
            CurveOutputRole::Rail(1),
            CurveOutputRole::Cap(0),
            CurveOutputRole::Cap(1),
        ])
    );
    assert_eq!(
        output
            .curves
            .iter()
            .filter(|curve| matches!(curve.geometry, CurveDraft2::CircularArc { .. }))
            .count(),
        2
    );
}

#[test]
fn resource_bounds_and_degenerate_geometry_are_rejected_before_staging() {
    let invalid_polygon = SketchRecipe::OuterDiameterPolygon {
        center: point(0.0, 0.0),
        outer_diameter: length(5.0),
        sides: SketchValue::Literal(Integer::new(257)),
        rotation: angle(0.0),
    };
    assert!(matches!(
        evaluate_recipe(
            &SketchDefinition::new(),
            &invalid_polygon,
            &SketchInputValues::default(),
            PrecisionPolicy::default()
        ),
        Err(SketchValidationError::PolygonSideCount { count: 257 })
    ));

    let outside = SketchRecipe::Line {
        start: point(0.0, 0.0),
        end: point(1.0e9 + 1.0, 0.0),
    };
    assert!(matches!(
        evaluate_recipe(
            &SketchDefinition::new(),
            &outside,
            &SketchInputValues::default(),
            PrecisionPolicy::default()
        ),
        Err(SketchValidationError::CoordinateOutOfBounds { .. })
    ));

    let degenerate = SketchRecipe::Line {
        start: point(1.0, 1.0),
        end: point(1.0, 1.0),
    };
    assert!(matches!(
        evaluate_recipe(
            &SketchDefinition::new(),
            &degenerate,
            &SketchInputValues::default(),
            PrecisionPolicy::default()
        ),
        Err(SketchValidationError::FeatureTooSmall { .. })
    ));
}

#[test]
fn centre_outer_slot_requires_material_between_its_caps() {
    let invalid = SketchRecipe::CentreOuterPointSlot {
        center: point(0.0, 0.0),
        overall_length: length(2.0),
        width: length(2.0),
        angle: angle(0.0),
    };
    assert!(matches!(
        evaluate_recipe(
            &SketchDefinition::new(),
            &invalid,
            &SketchInputValues::default(),
            PrecisionPolicy::default()
        ),
        Err(SketchValidationError::InvalidSlotDimensions)
    ));
}

#[test]
fn semantic_point_roles_do_not_depend_on_display_order() {
    let output = evaluate(&SketchRecipe::TwoPointRectangle {
        first_corner: point(5.0, 5.0),
        width: signed(-4.0),
        height: signed(3.0),
    });
    let roles = output
        .points
        .iter()
        .map(|point| point.role)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        roles,
        BTreeSet::from([
            PointOutputRole::Corner(0),
            PointOutputRole::Corner(1),
            PointOutputRole::Corner(2),
            PointOutputRole::Corner(3),
            PointOutputRole::Center,
        ])
    );
}

#[test]
fn invalid_typed_values_cannot_enter_through_serde() {
    let invalid_length: Length = serde_json::from_str("-2.0").expect("wire value decodes");
    let recipe = SketchRecipe::CentrePointCircle {
        center: point(0.0, 0.0),
        radius: SketchValue::Literal(invalid_length),
        radial_angle: angle(0.0),
    };
    assert!(matches!(
        evaluate_recipe(
            &SketchDefinition::new(),
            &recipe,
            &SketchInputValues::default(),
            PrecisionPolicy::default()
        ),
        Err(SketchValidationError::FeatureTooSmall { .. })
    ));
}

#[test]
fn definition_to_arrangement_adapter_excludes_construction_geometry() {
    let mut sketch = SketchDefinition::new();
    let rectangle = SketchRecipe::TwoPointRectangle {
        first_corner: point(0.0, 0.0),
        width: signed(8.0),
        height: signed(4.0),
    };
    let transaction = sketch.stage(rectangle, "Rectangle").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit rectangle");
    let centreline = SketchRecipe::CentreLine {
        start: point(-2.0, 2.0),
        end: point(10.0, 2.0),
    };
    let transaction = sketch.stage(centreline, "Centreline").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit centreline");

    let inputs = sketch.arrangement_inputs().expect("exact adapter");
    assert_eq!(inputs.len(), 4);
    let arrangement = build_arrangement(
        &inputs,
        &PrecisionPolicy::default(),
        ArrangementLimits::default(),
    );
    assert_eq!(arrangement.cells.len(), 1);
}

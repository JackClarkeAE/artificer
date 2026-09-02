//! Text as a sketch recipe: closed loops of exact lines that the arrangement
//! turns into selectable regions, replayed deterministically from intent.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    Angle, ArrangementLimits, ConfirmationSource, Length, PointInput, SketchDefinition,
    SketchEntityRole, SketchInputValues, SketchPoint2, SketchRecipe, SketchValidationError,
    SketchValue, build_arrangement, evaluate_recipe,
};

fn text(content: &str, height: f64, angle: f64) -> SketchRecipe {
    SketchRecipe::Text {
        anchor: PointInput::Position(SketchPoint2::new(3.0, 2.0)),
        content: content.to_owned(),
        height: SketchValue::Literal(Length::new(height).expect("height")),
        angle: SketchValue::Literal(Angle::radians(angle).expect("angle")),
    }
}

#[test]
fn a_letter_with_a_counter_becomes_a_ring_region_around_a_hole() {
    let mut sketch = SketchDefinition::new();
    let transaction = sketch.stage(text("O", 10.0, 0.0), "Text").expect("stage");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit text");
    let inputs = sketch.arrangement_inputs().expect("exact adapter");
    assert!(inputs.len() >= 16, "{} outline segments", inputs.len());
    let arrangement = build_arrangement(
        &inputs,
        &PrecisionPolicy::default(),
        ArrangementLimits::default(),
    );
    assert!(
        arrangement.diagnostics.is_empty(),
        "{:?}",
        arrangement.diagnostics
    );
    // The stroke of the O and its counter are two cells; the counter is the
    // one containing the letter's centre.
    assert_eq!(arrangement.cells.len(), 2, "ring and counter");
    let precision = PrecisionPolicy::default();
    let outlines = artificer_sketch::text::text_outlines("O", 10.0).unwrap();
    let centre_u = 3.0 + outlines.advance * 0.5;
    let counter = arrangement
        .cell_at_point(SketchPoint2::new(centre_u, 2.0 + 5.0), &precision)
        .expect("the counter is a cell");
    let stroke = arrangement
        .cell_at_point(SketchPoint2::new(centre_u, 2.0 + 0.3), &precision)
        .expect("the stroke is a cell");
    assert_ne!(counter.signature, stroke.signature);
}

#[test]
fn text_evaluates_deterministically_and_rotates_about_its_anchor() {
    let upright = evaluate_recipe(
        &SketchDefinition::new(),
        &text("Hi", 8.0, 0.0),
        &SketchInputValues::default(),
        PrecisionPolicy::default(),
    )
    .expect("upright text");
    let again = evaluate_recipe(
        &SketchDefinition::new(),
        &text("Hi", 8.0, 0.0),
        &SketchInputValues::default(),
        PrecisionPolicy::default(),
    )
    .expect("upright text again");
    assert_eq!(upright, again);
    assert!(
        upright
            .curves
            .iter()
            .all(|curve| curve.entity_role == SketchEntityRole::Profile)
    );
    // H, the dot of the i, and its stem: three loops, and every point of
    // every loop is a derived vertex the anchor binding precedes.
    assert!(upright.points.len() > upright.curves.len());

    let rotated = evaluate_recipe(
        &SketchDefinition::new(),
        &text("Hi", 8.0, std::f64::consts::FRAC_PI_2),
        &SketchInputValues::default(),
        PrecisionPolicy::default(),
    )
    .expect("rotated text");
    assert_eq!(rotated.curves.len(), upright.curves.len());
    // A quarter turn about the anchor sends every offset (du, dv) to
    // (-dv, du).
    for (up, rot) in upright.points.iter().zip(&rotated.points).skip(1) {
        let (du, dv) = (up.position.u - 3.0, up.position.v - 2.0);
        assert!((rot.position.u - (3.0 - dv)).abs() < 1.0e-9);
        assert!((rot.position.v - (2.0 + du)).abs() < 1.0e-9);
    }
}

#[test]
fn unsettable_text_is_refused_with_its_reason() {
    let error = evaluate_recipe(
        &SketchDefinition::new(),
        &text("   ", 8.0, 0.0),
        &SketchInputValues::default(),
        PrecisionPolicy::default(),
    )
    .expect_err("blank text has nothing to set");
    assert!(matches!(
        error,
        SketchValidationError::TextUnavailable { .. }
    ));
    assert!(error.to_string().contains("no visible characters"));
}

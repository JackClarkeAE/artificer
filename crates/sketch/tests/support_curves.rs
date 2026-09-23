//! The face boundary a sketch was drawn against travels with the sketch's
//! definition, so that the regions a canvas closed against it are the regions
//! a replay closes too.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ArrangementLimits, EvaluatedCurve2, SketchDefinition, SketchPoint2, build_arrangement,
};

fn square(half: f64) -> Vec<EvaluatedCurve2> {
    let corners = [
        SketchPoint2::new(-half, -half),
        SketchPoint2::new(half, -half),
        SketchPoint2::new(half, half),
        SketchPoint2::new(-half, half),
    ];
    (0..4)
        .map(|index| EvaluatedCurve2::Line {
            start: corners[index],
            end: corners[(index + 1) % 4],
        })
        .collect()
}

#[test]
fn support_curves_close_regions_and_carry_no_entity_of_their_own() {
    let mut definition = SketchDefinition::new();
    let revision = definition.revision();
    assert!(definition.set_support_curves(square(4.0)));
    assert!(
        !definition.set_support_curves(square(4.0)),
        "unchanged is unchanged"
    );
    assert_eq!(definition.revision(), revision, "context is not an edit");
    assert_eq!(definition.active_entities().count(), 0);

    let inputs = definition.arrangement_inputs().expect("valid");
    assert_eq!(inputs.len(), 4, "four sides of the face, and nothing else");
    assert!(
        inputs
            .iter()
            .all(|input| SketchDefinition::is_support_curve_entity(input.entity)),
        "each side names itself as the face's, not the sketch's"
    );
    let arrangement = build_arrangement(
        &inputs,
        &PrecisionPolicy::default(),
        ArrangementLimits::default(),
    );
    assert_eq!(
        arrangement.cells.len(),
        1,
        "the face is a region on its own"
    );
    assert!((arrangement.cells[0].signed_area.abs() - 64.0).abs() < 1.0e-9);
}

#[test]
fn support_curves_survive_the_document_and_close_the_same_regions_after_it() {
    let mut definition = SketchDefinition::new();
    definition.set_support_curves(square(4.0));
    let json = serde_json::to_string(&definition).expect("serialises");
    let restored: SketchDefinition = serde_json::from_str(&json).expect("deserialises");
    assert_eq!(restored.support_curves(), definition.support_curves());

    let precision = PrecisionPolicy::default();
    let before = build_arrangement(
        &definition.arrangement_inputs().expect("valid"),
        &precision,
        ArrangementLimits::default(),
    );
    let after = build_arrangement(
        &restored.arrangement_inputs().expect("valid"),
        &precision,
        ArrangementLimits::default(),
    );
    assert_eq!(
        before
            .cells
            .iter()
            .map(|cell| cell.signature.clone())
            .collect::<Vec<_>>(),
        after
            .cells
            .iter()
            .map(|cell| cell.signature.clone())
            .collect::<Vec<_>>(),
        "a replay names the same regions the canvas did"
    );
}

#[test]
fn a_definition_written_before_support_curves_still_loads() {
    let definition = SketchDefinition::new();
    let mut value: serde_json::Value = serde_json::to_value(&definition).expect("serialises");
    value
        .as_object_mut()
        .expect("an object")
        .remove("support_curves");
    let restored: SketchDefinition = serde_json::from_value(value).expect("an older document");
    assert!(restored.support_curves().is_empty());
}

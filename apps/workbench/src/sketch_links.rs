//! Sketch values that follow document variables (ADR 0054).
//!
//! A dimension typed as `width / 2` is kept by the sketch as a value link
//! beside the number it came to. The sketch never works a link out itself;
//! the document is what changes the variables, so this is where every sketch
//! that follows one is worked out again when one changes — in the app after
//! the Variables panel commits a value, and for a library part evaluated at
//! the values of one placement.

use std::collections::BTreeMap;

use artificer_model::{
    ModelDocument, ParameterOverrides, ParameterValue, QuantityKind, SketchId, SketchPayload,
};
use artificer_protocol::{PlanarProfile2, PrecisionPolicy};
use artificer_sketch::expression::{Dimension, NamedQuantity};
use artificer_sketch::{
    ArrangementLimits, SketchDefinition, build_arrangement, compile_selected_profile,
};
use artificer_sketch_ui::regenerate_linked_values;

use crate::authoring_region_signatures_for_profile;

/// The document's variables by name, evaluated and canonical — millimetres,
/// radians, or a bare number — each saying what it measures. This is what
/// every entry typed over a variable is read against.
#[must_use]
pub fn variable_values(document: &ModelDocument) -> BTreeMap<String, NamedQuantity> {
    let Ok(evaluated) = document.evaluate_parameters(&ParameterOverrides::default()) else {
        return BTreeMap::new();
    };
    document
        .parameters()
        .records()
        .iter()
        .filter_map(|record| {
            let ParameterValue::Quantity { value } = evaluated.get(record.id)? else {
                return None;
            };
            let dimension = match value.unit.quantity_kind() {
                QuantityKind::Length => Dimension::LENGTH,
                QuantityKind::Angle => Dimension::ANGLE,
                QuantityKind::Scalar => Dimension::SCALAR,
            };
            Some((
                record.spec.key.clone(),
                NamedQuantity {
                    canonical: value.magnitude,
                    dimension,
                },
            ))
        })
        .collect()
}

/// The payload a sketch comes to when its linked values follow `names`, or
/// `None` when they already agree with them.
///
/// The profile the payload carries is the regions it carried before — a
/// region keeps its signature when its sides move — compiled again at their
/// new size.
pub fn followed_sketch_payload(
    payload: &SketchPayload,
    names: &BTreeMap<String, NamedQuantity>,
    keep_points_connected: bool,
) -> Result<Option<SketchPayload>, String> {
    let Some(authoring) = payload.authoring() else {
        return Ok(None);
    };
    if authoring.value_links().is_empty() {
        return Ok(None);
    }
    let Some(followed) = regenerate_linked_values(authoring, names, keep_points_connected)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let profile = followed_profile(payload, authoring, &followed)?;
    SketchPayload::from_authoring(payload.frame, followed, profile, payload.support.clone())
        .map(Some)
        .map_err(|error| format!("the followed sketch is invalid: {error}"))
}

fn followed_profile(
    payload: &SketchPayload,
    before: &SketchDefinition,
    after: &SketchDefinition,
) -> Result<Option<PlanarProfile2>, String> {
    if payload.profile.regions.is_empty() {
        return Ok(None);
    }
    let lost = || "the sketch's profile no longer closes at the new values".to_owned();
    let regions =
        authoring_region_signatures_for_profile(before, &payload.profile).ok_or_else(lost)?;
    let precision = PrecisionPolicy::default();
    let inputs = after.arrangement_inputs().map_err(|_| lost())?;
    let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());
    compile_selected_profile(&arrangement, &regions, &precision)
        .map(|compiled| Some(compiled.profile))
        .map_err(|_| lost())
}

/// Works every sketch in `document` whose values follow its variables out
/// again at their current values, as part of the variable change that moved
/// them: no sketch adds an undo step of its own. Returns the sketches that
/// changed; the first one that cannot follow is an error naming it, and
/// sketches already followed stay followed — the caller abandons the change.
pub fn follow_variables(
    document: &mut ModelDocument,
    keep_points_connected: bool,
) -> Result<Vec<SketchId>, String> {
    let names = variable_values(document);
    let sketches = document
        .sketches()
        .iter()
        .map(|record| (record.id, record.label.clone(), record.geometry_revision))
        .collect::<Vec<_>>();
    let mut followed = Vec::new();
    for (sketch, label, revision) in sketches {
        let Some(payload) = document.sketch_payload(sketch, revision) else {
            continue;
        };
        let Some(payload) = followed_sketch_payload(payload, &names, keep_points_connected)
            .map_err(|error| format!("{label}: {error}"))?
        else {
            continue;
        };
        document
            .follow_variables_in_sketch(sketch, payload)
            .map_err(|error| format!("{label}: {error}"))?;
        followed.push(sketch);
    }
    Ok(followed)
}

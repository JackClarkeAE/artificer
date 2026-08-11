//! Native-v6 persistence evidence for the complete first-pass sketch recipe set.
//!
//! This target deliberately enters through public model/sketch APIs. It keeps
//! implementation tests separate from the UI acceptance suite and verifies
//! that a real native document retains editable intent, exact evaluated caches,
//! stable identities, and monotonic local allocation across a load boundary.

use std::collections::BTreeSet;
use std::f64::consts::{FRAC_PI_2, TAU};

use artificer_model::{
    CURRENT_DOCUMENT_VERSION, FeatureDraft, FeatureKind, ModelDocument, NativeDocument,
    OutputDraft, ReplayAction, SketchPayload, SketchSupportRecipe,
};
use artificer_protocol::{PlanarFrame3, Point3, PrecisionPolicy, Vector3};
use artificer_sketch::{
    Angle, CircularPatternDistribution, ConfirmationSource, CurveDirection, FilletBranchHints,
    Integer, Length, PointInput, SignedLength, SketchDefinition, SketchEntityId, SketchInputValues,
    SketchOperationId, SketchOutputRef, SketchPoint2, SketchRecipe, SketchUndoJournal, SketchValue,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

fn length(value: f64) -> SketchValue<Length> {
    SketchValue::Literal(Length::new(value).expect("positive finite length"))
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

fn line(start: (f64, f64), end: (f64, f64)) -> SketchRecipe {
    SketchRecipe::Line {
        start: point(start.0, start.1),
        end: point(end.0, end.1),
    }
}

fn commit_creation(
    sketch: &mut SketchDefinition,
    recipe: SketchRecipe,
    label: &str,
) -> SketchOperationId {
    let transaction = sketch.stage(recipe, label).expect("stage creation recipe");
    let operation = transaction
        .impact()
        .inserted_operations
        .first()
        .copied()
        .expect("creation inserts one operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit creation recipe");
    operation
}

fn commit_modifier(
    sketch: &mut SketchDefinition,
    recipe: SketchRecipe,
    label: &str,
) -> SketchOperationId {
    let transaction = sketch
        .stage_modifier(recipe, label)
        .expect("stage modifier recipe");
    let operation = transaction
        .impact()
        .inserted_operations
        .first()
        .copied()
        .expect("modifier inserts one operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit modifier recipe");
    operation
}

fn first_curve(sketch: &SketchDefinition, operation: SketchOperationId) -> SketchEntityId {
    sketch
        .operation(operation)
        .expect("operation exists")
        .outputs
        .values()
        .find_map(|output| match output {
            SketchOutputRef::Curve(entity) => Some(*entity),
            SketchOutputRef::Point(_) => None,
        })
        .expect("operation owns a curve")
}

fn origin_frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::default(),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

struct RecipeMatrix {
    definition: SketchDefinition,
    replay_seed: SketchOperationId,
}

fn full_recipe_matrix() -> RecipeMatrix {
    let mut sketch = SketchDefinition::new();

    commit_creation(
        &mut sketch,
        SketchRecipe::Point {
            position: SketchPoint2::new(95.0, 0.0),
        },
        "Reference point",
    );
    let replay_seed = commit_creation(&mut sketch, line((100.0, 0.0), (102.0, 0.0)), "Single line");
    commit_creation(
        &mut sketch,
        SketchRecipe::CentreLine {
            start: point(100.0, 5.0),
            end: point(104.0, 5.0),
        },
        "Centre line",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::Polyline {
            vertices: vec![point(100.0, 10.0), point(102.0, 11.0), point(104.0, 10.0)],
            closed: false,
            construction: false,
        },
        "Open chained polyline",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::Polyline {
            vertices: vec![point(100.0, 15.0), point(104.0, 15.0), point(102.0, 18.0)],
            closed: true,
            construction: false,
        },
        "Closed chained polyline",
    );

    commit_creation(
        &mut sketch,
        SketchRecipe::TwoPointRectangle {
            first_corner: point(0.0, 0.0),
            width: signed(4.0),
            height: signed(3.0),
        },
        "Two-point rectangle",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::CentrePointRectangle {
            center: point(10.0, 0.0),
            width: length(4.0),
            height: length(2.0),
        },
        "Centre-point rectangle",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::CentrePointCircle {
            center: point(20.0, 0.0),
            radius: length(2.0),
            radial_angle: angle(FRAC_PI_2),
        },
        "Centre-radius circle",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::TwoPointCircle {
            first_diameter_point: point(28.0, 0.0),
            second_diameter_point: point(32.0, 0.0),
            direction: CurveDirection::Clockwise,
        },
        "Two-point circle",
    );

    // Both UI arc gestures canonicalize to the same exact analytic recipe.
    // Exercise both orientations so native persistence proves the complete
    // carrier domain without relying on tessellation.
    commit_creation(
        &mut sketch,
        SketchRecipe::CentreStartEndArc {
            center: point(40.0, 0.0),
            start: point(42.0, 0.0),
            end: point(40.0, 2.0),
            direction: CurveDirection::CounterClockwise,
        },
        "Centre-start-end arc",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::CentreStartEndArc {
            center: point(50.0, 0.0),
            start: point(52.0, 0.0),
            end: point(50.0, -2.0),
            direction: CurveDirection::Clockwise,
        },
        "Three-point arc (canonical analytic)",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::InnerDiameterPolygon {
            center: point(0.0, 12.0),
            inner_diameter: length(4.0),
            sides: count(5),
            rotation: angle(0.25),
        },
        "Inner-diameter polygon",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::OuterDiameterPolygon {
            center: point(12.0, 12.0),
            outer_diameter: length(5.0),
            sides: count(6),
            rotation: angle(0.5),
        },
        "Outer-diameter polygon",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::TwoPointSlot {
            first_cap_center: point(22.0, 12.0),
            second_cap_center: point(28.0, 12.0),
            width: length(2.0),
        },
        "Two-point slot",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::CentreOuterPointSlot {
            center: point(40.0, 12.0),
            overall_length: length(8.0),
            width: length(2.0),
            angle: angle(0.35),
        },
        "Centre-to-outer-point slot",
    );

    let pattern_seed = first_curve(&sketch, replay_seed);
    commit_creation(
        &mut sketch,
        SketchRecipe::RectangularPattern {
            sources: vec![pattern_seed],
            columns: count(3),
            rows: count(2),
            column_spacing: signed(8.0),
            row_spacing: signed(6.0),
            direction: angle(0.1),
        },
        "Rectangular pattern",
    );
    commit_creation(
        &mut sketch,
        SketchRecipe::CircularPattern {
            sources: vec![pattern_seed],
            center: point(100.0, -10.0),
            count: count(4),
            total_angle: angle(TAU),
            distribution: CircularPatternDistribution::Complete,
            rotate_instances: true,
        },
        "Circular pattern",
    );

    let trim_target = commit_creation(&mut sketch, line((-4.0, 30.0), (4.0, 30.0)), "Trim target");
    let trim_limit_a = commit_creation(
        &mut sketch,
        line((-1.0, 28.0), (-1.0, 32.0)),
        "Trim limit A",
    );
    let trim_limit_b = commit_creation(&mut sketch, line((1.0, 28.0), (1.0, 32.0)), "Trim limit B");
    let trim = sketch
        .stage_trim(
            first_curve(&sketch, trim_target),
            vec![
                first_curve(&sketch, trim_limit_b),
                first_curve(&sketch, trim_limit_a),
            ],
            SketchPoint2::new(0.0, 30.0),
            "Trim enclosed span",
            PrecisionPolicy::default(),
        )
        .expect("stage exact trim");
    sketch
        .commit(trim, ConfirmationSource::GreenTick)
        .expect("commit exact trim");

    let fillet_first = commit_creation(
        &mut sketch,
        line((-10.0, 50.0), (0.0, 50.0)),
        "Fillet source A",
    );
    let fillet_second = commit_creation(
        &mut sketch,
        line((0.0, 50.0), (0.0, 60.0)),
        "Fillet source B",
    );
    let fillet_first_curve = first_curve(&sketch, fillet_first);
    let fillet_second_curve = first_curve(&sketch, fillet_second);
    commit_modifier(
        &mut sketch,
        SketchRecipe::FilletWithHints {
            first: fillet_first_curve,
            second: fillet_second_curve,
            radius: length(1.5),
            hints: FilletBranchHints {
                first_pick: SketchPoint2::new(-5.0, 50.0),
                second_pick: SketchPoint2::new(0.0, 55.0),
                corner_hint: SketchPoint2::new(0.0, 50.0),
            },
        },
        "Fillet",
    );

    let equal_first = commit_creation(
        &mut sketch,
        line((10.0, 50.0), (20.0, 50.0)),
        "Equal chamfer source A",
    );
    let equal_second = commit_creation(
        &mut sketch,
        line((20.0, 50.0), (20.0, 60.0)),
        "Equal chamfer source B",
    );
    let equal_first_curve = first_curve(&sketch, equal_first);
    let equal_second_curve = first_curve(&sketch, equal_second);
    commit_modifier(
        &mut sketch,
        SketchRecipe::Chamfer {
            first: equal_first_curve,
            second: equal_second_curve,
            first_distance: length(2.0),
            second_distance: length(2.0),
        },
        "Equal-distance chamfer",
    );

    let unequal_first = commit_creation(
        &mut sketch,
        line((30.0, 50.0), (40.0, 50.0)),
        "Two-distance chamfer source A",
    );
    let unequal_second = commit_creation(
        &mut sketch,
        line((40.0, 50.0), (40.0, 60.0)),
        "Two-distance chamfer source B",
    );
    let unequal_first_curve = first_curve(&sketch, unequal_first);
    let unequal_second_curve = first_curve(&sketch, unequal_second);
    commit_modifier(
        &mut sketch,
        SketchRecipe::Chamfer {
            first: unequal_first_curve,
            second: unequal_second_curve,
            first_distance: length(1.0),
            second_distance: length(3.0),
        },
        "Two-distance chamfer",
    );

    sketch
        .validate(PrecisionPolicy::default())
        .expect("complete recipe matrix replays");
    RecipeMatrix {
        definition: sketch,
        replay_seed,
    }
}

fn assert_first_pass_recipe_coverage(sketch: &SketchDefinition) {
    let operations = sketch.operations();
    let count_matching = |predicate: fn(&SketchRecipe) -> bool| {
        operations
            .iter()
            .filter(|operation| predicate(&operation.recipe))
            .count()
    };

    assert_eq!(
        count_matching(|recipe| matches!(recipe, SketchRecipe::Line { .. })),
        10
    );
    assert_eq!(
        count_matching(|recipe| matches!(recipe, SketchRecipe::CentreLine { .. })),
        1
    );
    assert_eq!(
        count_matching(|recipe| matches!(recipe, SketchRecipe::Polyline { closed: false, .. })),
        1
    );
    assert_eq!(
        count_matching(|recipe| matches!(recipe, SketchRecipe::Polyline { closed: true, .. })),
        1
    );
    let singleton_predicates: [fn(&SketchRecipe) -> bool; 13] = [
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::Point { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::TwoPointRectangle { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::CentrePointRectangle { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::CentrePointCircle { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::TwoPointCircle { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::InnerDiameterPolygon { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::OuterDiameterPolygon { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::TwoPointSlot { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::CentreOuterPointSlot { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::RectangularPattern { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::CircularPattern { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::FilletWithHints { .. }),
        |recipe: &SketchRecipe| matches!(recipe, SketchRecipe::Trim { .. }),
    ];
    for predicate in singleton_predicates {
        assert_eq!(count_matching(predicate), 1);
    }
    assert_eq!(
        count_matching(|recipe| matches!(recipe, SketchRecipe::CentreStartEndArc { .. })),
        2,
        "centre-start-end and three-point gestures share one exact arc carrier recipe"
    );
    assert_eq!(
        count_matching(|recipe| matches!(recipe, SketchRecipe::Chamfer { .. })),
        2,
        "equal and independent-distance chamfers persist through one general recipe"
    );
    assert!(operations.iter().any(|operation| matches!(
        operation.recipe,
        SketchRecipe::CentreStartEndArc {
            direction: CurveDirection::CounterClockwise,
            ..
        }
    )));
    assert!(operations.iter().any(|operation| matches!(
        operation.recipe,
        SketchRecipe::CentreStartEndArc {
            direction: CurveDirection::Clockwise,
            ..
        }
    )));
}

#[test]
fn every_first_pass_recipe_round_trips_in_a_native_v6_document_with_exact_ids_and_geometry() {
    let matrix = full_recipe_matrix();
    assert_first_pass_recipe_coverage(&matrix.definition);

    let operation_ids = matrix
        .definition
        .operations()
        .iter()
        .map(|operation| operation.id)
        .collect::<Vec<_>>();
    let point_ids = matrix
        .definition
        .points()
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let entity_ids = matrix
        .definition
        .entities()
        .keys()
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        operation_ids.iter().copied().collect::<BTreeSet<_>>().len(),
        operation_ids.len()
    );
    assert_eq!(
        point_ids.iter().copied().collect::<BTreeSet<_>>().len(),
        point_ids.len()
    );
    assert_eq!(
        entity_ids.iter().copied().collect::<BTreeSet<_>>().len(),
        entity_ids.len()
    );
    let evaluated_geometry = matrix
        .definition
        .active_entities()
        .map(|entity| {
            (
                entity.id,
                matrix
                    .definition
                    .evaluated_curve(entity.id)
                    .expect("active entity evaluates"),
            )
        })
        .collect::<Vec<_>>();
    let high_water = matrix.definition.high_water_marks();
    assert_eq!(high_water.operation(), operation_ids.last().unwrap().get());
    assert_eq!(high_water.point(), point_ids.last().unwrap().get());
    assert_eq!(high_water.entity(), entity_ids.last().unwrap().get());

    let payload = SketchPayload::from_authoring(
        origin_frame(),
        matrix.definition.clone(),
        None,
        SketchSupportRecipe::Origin,
    )
    .expect("complete editable definition is a valid portable payload");
    let mut document = ModelDocument::default();
    let appended = document
        .append_feature(
            FeatureDraft::new(
                FeatureKind::Sketch,
                "First-pass recipe matrix",
                ReplayAction::Marker,
            )
            .with_sketch_payload(payload)
            .with_output(OutputDraft::CreateSketch {
                label: "First-pass recipe matrix".to_owned(),
                geometry_revision: matrix.definition.revision().get(),
            }),
        )
        .expect("append native editable sketch");
    let sketch_id = appended.created_sketches[0];
    let native = document.to_native();
    assert_eq!(native.version(), CURRENT_DOCUMENT_VERSION);

    let encoded = serde_json::to_string(&native).expect("serialize native v6 envelope");
    let decoded: NativeDocument =
        serde_json::from_str(&encoded).expect("decode native v6 envelope");
    let restored = ModelDocument::from_native(decoded).expect("validate native v6 archive");
    assert_eq!(restored.to_native(), native);
    assert_eq!(
        serde_json::to_string(&restored.to_native()).expect("reserialize native v6 envelope"),
        encoded,
        "native v6 output is byte-deterministic for a fixed semantic document"
    );

    let restored_record = restored
        .sketch(sketch_id)
        .expect("restored stable sketch ID");
    let restored_authoring = restored
        .sketch_payload(sketch_id, restored_record.geometry_revision)
        .and_then(SketchPayload::authoring)
        .expect("v6 requires editable authoring");
    assert_eq!(restored_authoring, &matrix.definition);
    assert_eq!(restored_authoring.high_water_marks(), high_water);
    assert_eq!(
        restored_authoring
            .operations()
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>(),
        operation_ids
    );
    assert_eq!(
        restored_authoring
            .points()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        point_ids
    );
    assert_eq!(
        restored_authoring
            .entities()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        entity_ids
    );
    assert_eq!(
        restored_authoring
            .active_entities()
            .map(|entity| {
                (
                    entity.id,
                    restored_authoring
                        .evaluated_curve(entity.id)
                        .expect("restored entity evaluates"),
                )
            })
            .collect::<Vec<_>>(),
        evaluated_geometry
    );
    restored_authoring
        .validate(PrecisionPolicy::default())
        .expect("loaded recipe graph replays from intent");
}

#[test]
fn loaded_v6_authoring_supports_dependent_edit_document_undo_and_monotonic_local_undo() {
    let matrix = full_recipe_matrix();
    let payload = SketchPayload::from_authoring(
        origin_frame(),
        matrix.definition.clone(),
        None,
        SketchSupportRecipe::Origin,
    )
    .expect("editable payload");
    let mut source = ModelDocument::default();
    let appended = source
        .append_feature(
            FeatureDraft::new(FeatureKind::Sketch, "Editable matrix", ReplayAction::Marker)
                .with_sketch_payload(payload)
                .with_output(OutputDraft::CreateSketch {
                    label: "Editable matrix".to_owned(),
                    geometry_revision: matrix.definition.revision().get(),
                }),
        )
        .expect("append matrix");
    let sketch_id = appended.created_sketches[0];
    let native_json = serde_json::to_string(&source.to_native()).expect("save v6");
    let mut loaded: ModelDocument = serde_json::from_str(&native_json).expect("load v6");
    assert!(
        !loaded.can_undo(),
        "the runtime journal is intentionally not serialized"
    );

    let loaded_revision = loaded.sketch(sketch_id).unwrap().geometry_revision;
    let loaded_definition = loaded
        .sketch_payload(sketch_id, loaded_revision)
        .and_then(SketchPayload::authoring)
        .expect("loaded authoring")
        .clone();
    let original_pattern_outputs = loaded_definition
        .operations()
        .iter()
        .filter(|operation| {
            matches!(
                operation.recipe,
                SketchRecipe::RectangularPattern { .. } | SketchRecipe::CircularPattern { .. }
            )
        })
        .map(|operation| (operation.id, operation.outputs.clone()))
        .collect::<Vec<_>>();
    let replacement = loaded_definition
        .stage_replace(
            matrix.replay_seed,
            line((100.0, 0.0), (105.0, 0.0)),
            "Resize shared pattern seed",
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("replay both dependent pattern branches after load");
    let mut edited_definition = loaded_definition.clone();
    edited_definition
        .commit(replacement, ConfirmationSource::GreenTick)
        .expect("commit post-load authoring edit");
    for (operation, outputs) in &original_pattern_outputs {
        assert_eq!(
            &edited_definition.operation(*operation).unwrap().outputs,
            outputs,
            "dependent replay retains stable output entity IDs"
        );
    }
    assert_eq!(
        edited_definition.high_water_marks(),
        loaded_definition.high_water_marks(),
        "same-role replay requires no fresh identities"
    );

    let edited_payload = SketchPayload::from_authoring(
        origin_frame(),
        edited_definition.clone(),
        None,
        SketchSupportRecipe::Origin,
    )
    .expect("edited payload");
    assert!(
        loaded
            .replace_sketch_payload(sketch_id, edited_payload)
            .expect("publish post-load edit")
    );
    let edited_revision = loaded.sketch(sketch_id).unwrap().geometry_revision;
    assert_eq!(edited_revision, loaded_revision + 1);
    assert_eq!(
        loaded
            .sketch_payload(sketch_id, edited_revision)
            .and_then(SketchPayload::authoring),
        Some(&edited_definition)
    );
    assert!(
        loaded.undo(),
        "first post-load edit creates one document checkpoint"
    );
    assert_eq!(
        loaded.sketch(sketch_id).unwrap().geometry_revision,
        loaded_revision
    );
    assert_eq!(
        loaded
            .sketch_payload(sketch_id, loaded_revision)
            .and_then(SketchPayload::authoring),
        Some(&loaded_definition)
    );
    assert!(loaded.redo());
    assert_eq!(
        loaded
            .sketch_payload(sketch_id, edited_revision)
            .and_then(SketchPayload::authoring),
        Some(&edited_definition)
    );

    // Local sketch undo restores topology but keeps every identity that was
    // already published after load above the allocator high-water mark.
    let mut local = loaded_definition.clone();
    let before_counts = (
        local.operations().len(),
        local.points().len(),
        local.entities().len(),
    );
    let staged = local
        .stage(line((200.0, 0.0), (203.0, 0.0)), "Post-load local line")
        .expect("stage local edit");
    let inserted_operation = *staged
        .impact()
        .inserted_operations
        .first()
        .expect("one inserted operation");
    let inserted_entity = *staged
        .impact()
        .inserted_entities
        .first()
        .expect("one inserted entity");
    let mut journal = SketchUndoJournal::new(4);
    journal
        .confirm(
            &mut local,
            staged,
            ConfirmationSource::BareEnter,
            PrecisionPolicy::default(),
        )
        .expect("confirm post-load local edit");
    let published_marks = local.high_water_marks();
    assert_eq!(published_marks.operation(), inserted_operation.get());
    assert_eq!(published_marks.entity(), inserted_entity.get());
    assert!(journal.undo(&mut local));
    assert_eq!(
        (
            local.operations().len(),
            local.points().len(),
            local.entities().len(),
        ),
        before_counts
    );
    assert_eq!(local.high_water_marks(), published_marks);

    let replacement_identity = local
        .stage(line((210.0, 0.0), (213.0, 0.0)), "Identity after undo")
        .expect("stage after undo");
    assert!(
        replacement_identity
            .impact()
            .inserted_operations
            .first()
            .expect("replacement operation")
            .get()
            > inserted_operation.get()
    );
    assert!(
        replacement_identity
            .impact()
            .inserted_entities
            .first()
            .expect("replacement entity")
            .get()
            > inserted_entity.get()
    );
    assert_eq!(
        replacement_identity.cancel().unchanged_revision,
        local.revision()
    );
}

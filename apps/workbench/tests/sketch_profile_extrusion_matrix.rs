//! Recipe-to-region-to-kernel evidence for the first-pass closed-profile domain.
//!
//! Every case starts with a native editable recipe, selects an analytic
//! arrangement cell by model-space point, verifies that the selected boundary
//! carries an entity emitted by the feature recipe under test, compiles a
//! generic `PlanarProfile2`, and executes the public new-body command twice.

use std::collections::BTreeSet;
use std::f64::consts::{FRAC_PI_4, PI};

use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, PlanarCurve2, PlanarFrame3, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};
use artificer_sketch::{
    Angle, ArrangementLimits, ConfirmationSource, CurveOutputRole, FilletBranchHints, Integer,
    Length, OutputRole, PointInput, SignedLength, SketchDefinition, SketchEntityId,
    SketchOperationId, SketchOutputRef, SketchPoint2, SketchRecipe, SketchValue, build_arrangement,
    compile_selected_profile,
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
    let transaction = sketch.stage(recipe, label).expect("stage creation");
    let operation = *transaction
        .impact()
        .inserted_operations
        .first()
        .expect("one inserted operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit creation");
    operation
}

fn commit_modifier(
    sketch: &mut SketchDefinition,
    recipe: SketchRecipe,
    label: &str,
) -> SketchOperationId {
    let transaction = sketch
        .stage_modifier(recipe, label)
        .expect("stage modifier");
    let operation = *transaction
        .impact()
        .inserted_operations
        .first()
        .expect("one inserted operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit modifier");
    operation
}

fn curve_by_role(
    sketch: &SketchDefinition,
    operation: SketchOperationId,
    role: CurveOutputRole,
) -> SketchEntityId {
    match sketch
        .operation(operation)
        .expect("operation exists")
        .outputs[&OutputRole::Curve(role)]
    {
        SketchOutputRef::Curve(entity) => entity,
        SketchOutputRef::Point(_) => panic!("curve role resolved to a point"),
    }
}

fn active_operation_curves(
    sketch: &SketchDefinition,
    operation: SketchOperationId,
) -> BTreeSet<SketchEntityId> {
    sketch
        .operation(operation)
        .expect("operation exists")
        .outputs
        .values()
        .filter_map(|output| match output {
            SketchOutputRef::Curve(entity)
                if sketch.entity(*entity).is_some_and(|record| record.active) =>
            {
                Some(*entity)
            }
            SketchOutputRef::Curve(_) | SketchOutputRef::Point(_) => None,
        })
        .collect()
}

struct ProfileFixture {
    name: &'static str,
    sketch: SketchDefinition,
    selected_points: Vec<SketchPoint2>,
    provenance_operation: SketchOperationId,
    expected_regions: usize,
    expected_kinds: (usize, usize, usize),
    distance: f64,
    expected_volume: f64,
}

fn polygon_fixture() -> ProfileFixture {
    let mut sketch = SketchDefinition::new();
    let operation = commit_creation(
        &mut sketch,
        SketchRecipe::OuterDiameterPolygon {
            center: point(0.0, 0.0),
            outer_diameter: length(2.0),
            sides: count(4),
            rotation: angle(FRAC_PI_4),
        },
        "Square by outer diameter",
    );
    ProfileFixture {
        name: "polygon",
        sketch,
        selected_points: vec![SketchPoint2::new(0.0, 0.0)],
        provenance_operation: operation,
        expected_regions: 1,
        expected_kinds: (4, 0, 0),
        distance: 3.0,
        expected_volume: 6.0,
    }
}

fn slot_fixture() -> ProfileFixture {
    let mut sketch = SketchDefinition::new();
    let operation = commit_creation(
        &mut sketch,
        SketchRecipe::TwoPointSlot {
            first_cap_center: point(-2.0, 0.0),
            second_cap_center: point(2.0, 0.0),
            width: length(2.0),
        },
        "Analytic slot",
    );
    ProfileFixture {
        name: "slot",
        sketch,
        selected_points: vec![SketchPoint2::new(0.0, 0.0)],
        provenance_operation: operation,
        expected_regions: 1,
        expected_kinds: (2, 2, 0),
        distance: 2.0,
        expected_volume: 16.0 + 2.0 * PI,
    }
}

fn trimmed_cell_fixture() -> ProfileFixture {
    let mut sketch = SketchDefinition::new();
    let rectangle = commit_creation(
        &mut sketch,
        SketchRecipe::TwoPointRectangle {
            first_corner: point(0.0, 0.0),
            width: signed(4.0),
            height: signed(4.0),
        },
        "Trim cell boundary",
    );
    let target = commit_creation(
        &mut sketch,
        line((-1.0, 2.0), (4.0, 2.0)),
        "Trimmed divider",
    );
    let right = curve_by_role(&sketch, rectangle, CurveOutputRole::Side(1));
    let left = curve_by_role(&sketch, rectangle, CurveOutputRole::Side(3));
    let target_curve = curve_by_role(&sketch, target, CurveOutputRole::Curve);
    let transaction = sketch
        .stage_trim(
            target_curve,
            vec![right, left],
            SketchPoint2::new(-0.5, 2.0),
            "Trim divider to its cell span",
            PrecisionPolicy::default(),
        )
        .expect("trim the outer divider span");
    let operation = *transaction
        .impact()
        .inserted_operations
        .first()
        .expect("one inserted trim operation");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit trimmed divider");
    ProfileFixture {
        name: "trimmed cell",
        sketch,
        selected_points: vec![SketchPoint2::new(2.0, 1.0)],
        provenance_operation: operation,
        expected_regions: 1,
        expected_kinds: (4, 0, 0),
        distance: 2.0,
        expected_volume: 16.0,
    }
}

fn patterned_regions_fixture() -> ProfileFixture {
    let mut sketch = SketchDefinition::new();
    let rectangle = commit_creation(
        &mut sketch,
        SketchRecipe::CentrePointRectangle {
            center: point(0.0, 0.0),
            width: length(2.0),
            height: length(2.0),
        },
        "Pattern seed",
    );
    let sources = (0..4)
        .map(|index| curve_by_role(&sketch, rectangle, CurveOutputRole::Side(index)))
        .collect();
    let pattern = commit_creation(
        &mut sketch,
        SketchRecipe::RectangularPattern {
            sources,
            columns: count(2),
            rows: count(1),
            column_spacing: signed(5.0),
            row_spacing: signed(0.0),
            direction: angle(0.0),
        },
        "Pattern closed region",
    );
    ProfileFixture {
        name: "patterned closed regions",
        sketch,
        selected_points: vec![SketchPoint2::new(0.0, 0.0), SketchPoint2::new(5.0, 0.0)],
        provenance_operation: pattern,
        expected_regions: 2,
        expected_kinds: (8, 0, 0),
        distance: 1.5,
        expected_volume: 12.0,
    }
}

fn filleted_loop_fixture() -> ProfileFixture {
    let mut sketch = SketchDefinition::new();
    let rectangle = commit_creation(
        &mut sketch,
        SketchRecipe::CentrePointRectangle {
            center: point(0.0, 0.0),
            width: length(4.0),
            height: length(4.0),
        },
        "Fillet loop",
    );
    let bottom = curve_by_role(&sketch, rectangle, CurveOutputRole::Side(0));
    let right = curve_by_role(&sketch, rectangle, CurveOutputRole::Side(1));
    let fillet = commit_modifier(
        &mut sketch,
        SketchRecipe::FilletWithHints {
            first: bottom,
            second: right,
            radius: length(1.0),
            hints: FilletBranchHints {
                first_pick: SketchPoint2::new(0.0, -2.0),
                second_pick: SketchPoint2::new(2.0, 0.0),
                corner_hint: SketchPoint2::new(2.0, -2.0),
            },
        },
        "Fillet loop corner",
    );
    ProfileFixture {
        name: "filleted loop",
        sketch,
        selected_points: vec![SketchPoint2::new(0.0, 0.0)],
        provenance_operation: fillet,
        expected_regions: 1,
        expected_kinds: (4, 1, 0),
        distance: 2.0,
        expected_volume: 30.0 + PI / 2.0,
    }
}

fn chamfered_loop_fixture() -> ProfileFixture {
    let mut sketch = SketchDefinition::new();
    let rectangle = commit_creation(
        &mut sketch,
        SketchRecipe::CentrePointRectangle {
            center: point(0.0, 0.0),
            width: length(4.0),
            height: length(4.0),
        },
        "Chamfer loop",
    );
    let bottom = curve_by_role(&sketch, rectangle, CurveOutputRole::Side(0));
    let right = curve_by_role(&sketch, rectangle, CurveOutputRole::Side(1));
    let chamfer = commit_modifier(
        &mut sketch,
        SketchRecipe::Chamfer {
            first: bottom,
            second: right,
            first_distance: length(1.0),
            second_distance: length(2.0),
        },
        "Two-distance chamfer loop corner",
    );
    ProfileFixture {
        name: "chamfered loop",
        sketch,
        selected_points: vec![SketchPoint2::new(0.0, 0.0)],
        provenance_operation: chamfer,
        expected_regions: 1,
        expected_kinds: (5, 0, 0),
        distance: 2.0,
        expected_volume: 30.0,
    }
}

fn profile_curve_kinds(profile: &artificer_protocol::PlanarProfile2) -> (usize, usize, usize) {
    let mut kinds = (0, 0, 0);
    for curve in profile.regions.iter().flat_map(|region| {
        std::iter::once(&region.outer)
            .chain(region.holes.iter())
            .flat_map(|profile_loop| profile_loop.curves.iter())
    }) {
        match curve {
            PlanarCurve2::Line { .. } => kinds.0 += 1,
            PlanarCurve2::CircularArc { .. } => kinds.1 += 1,
            PlanarCurve2::Circle { .. } => kinds.2 += 1,
            PlanarCurve2::Bspline { .. } => {}
        }
    }
    kinds
}

fn frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::default(),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

fn assert_close(name: &str, actual: f64, expected: f64) {
    let tolerance = 1.0e-9 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{name}: expected {expected:.17e}, got {actual:.17e}"
    );
}

#[test]
fn recipe_origin_profiles_extrude_exactly_through_the_generic_new_body_command() {
    let fixtures = [
        polygon_fixture(),
        slot_fixture(),
        trimmed_cell_fixture(),
        patterned_regions_fixture(),
        filleted_loop_fixture(),
        chamfered_loop_fixture(),
    ];
    let precision = PrecisionPolicy::default();

    for fixture in fixtures {
        fixture
            .sketch
            .validate(precision)
            .unwrap_or_else(|error| panic!("{} intent replay failed: {error}", fixture.name));
        let inputs = fixture
            .sketch
            .arrangement_inputs()
            .unwrap_or_else(|error| panic!("{} arrangement input failed: {error}", fixture.name));
        let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());
        assert!(
            arrangement.diagnostics.is_empty(),
            "{} arrangement diagnostics: {:#?}",
            fixture.name,
            arrangement.diagnostics
        );
        let signatures = fixture
            .selected_points
            .iter()
            .map(|selection| {
                arrangement
                    .cell_at_point(*selection, &precision)
                    .unwrap_or_else(|| panic!("{} selection {selection:?} missed", fixture.name))
                    .signature
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            signatures.iter().collect::<BTreeSet<_>>().len(),
            signatures.len(),
            "{} selection points must identify distinct cells",
            fixture.name
        );

        let boundary_sources = signatures
            .iter()
            .flat_map(|signature| {
                signature
                    .outer
                    .iter()
                    .chain(signature.holes.iter().flatten())
            })
            .map(|fragment| fragment.source_entity)
            .collect::<BTreeSet<_>>();
        let feature_outputs =
            active_operation_curves(&fixture.sketch, fixture.provenance_operation);
        assert!(
            !feature_outputs.is_disjoint(&boundary_sources),
            "{} selected boundary lost provenance from operation {}",
            fixture.name,
            fixture.provenance_operation
        );

        let compiled = compile_selected_profile(&arrangement, &signatures, &precision)
            .unwrap_or_else(|error| panic!("{} profile compile failed: {error}", fixture.name));
        assert_eq!(compiled.selected_regions.len(), signatures.len());
        assert_eq!(compiled.profile.regions.len(), fixture.expected_regions);
        assert_eq!(
            profile_curve_kinds(&compiled.profile),
            fixture.expected_kinds,
            "{} must preserve analytic curve kinds",
            fixture.name
        );

        let input = NativeKernel::empty();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("recipe-profile-{}", fixture.name)),
            expected_snapshot: input.id(),
            precision,
            command: KernelCommand::ExtrudePlanarProfile {
                frame: frame(),
                profile: compiled.profile,
                distance: fixture.distance,
            },
        };
        let first = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .unwrap_or_else(|error| panic!("{} extrusion failed: {error:#?}", fixture.name));
        let replay = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .unwrap_or_else(|error| panic!("{} replay failed: {error:#?}", fixture.name));
        assert_eq!(
            first.snapshot.id(),
            replay.snapshot.id(),
            "{}",
            fixture.name
        );
        assert_eq!(
            first.snapshot.semantic_digest(),
            replay.snapshot.semantic_digest(),
            "{} semantic digest must be replay-stable",
            fixture.name
        );
        assert_close(
            fixture.name,
            first.snapshot.measures().volume,
            fixture.expected_volume,
        );
        assert!(first.report.validation.valid, "{}", fixture.name);
        assert!(
            NativeKernel::validate(&first.snapshot, ValidationProfile::Solid).valid,
            "{}",
            fixture.name
        );
        assert_eq!(
            first.report.history.len(),
            first.snapshot.counts().total() as usize,
            "{} topology history must cover every emitted entity",
            fixture.name
        );
    }
}

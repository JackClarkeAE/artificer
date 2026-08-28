use artificer_protocol::{PlanarCurve2, PrecisionPolicy};
use artificer_sketch::{
    ConfirmationSource, EvaluatedCurve2, PointInput, SketchConstraintKind, SketchDefinition,
    SketchPoint2, SketchRecipe, build_arrangement, compile_selected_profile, intersect_curves,
};

fn point(u: f64, v: f64) -> PointInput {
    PointInput::Position(SketchPoint2::new(u, v))
}

#[test]
fn fit_point_spline_recipe_interpolates_authored_fit_points() {
    let mut sketch = SketchDefinition::new();
    let fit_pts = vec![
        point(0.0, 0.0),
        point(10.0, 5.0),
        point(20.0, -5.0),
        point(30.0, 0.0),
    ];
    let recipe = SketchRecipe::FitPointSpline {
        fit_points: fit_pts,
        degree: 3,
        closed: false,
    };
    let transaction = sketch
        .stage(recipe, "Fit Spline")
        .expect("stage fit spline");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit fit spline");

    // 4 fit points + 4 computed control points = 8 active points
    assert_eq!(sketch.active_points().count(), 8);
    assert_eq!(sketch.active_entities().count(), 1);

    let curve_id = sketch.active_entities().next().unwrap().id;
    let eval = sketch.evaluated_curve(curve_id).expect("evaluated spline");

    let EvaluatedCurve2::Bspline {
        control_points,
        degree,
        knots,
        weights,
    } = eval
    else {
        panic!("expected evaluated bspline");
    };

    assert_eq!(degree, 3);
    assert!(weights.is_none());
    assert_eq!(knots.len(), control_points.len() + degree + 1);

    // Verify interpolation at u=0.0 and u=1.0
    let start = sketch
        .evaluated_curve(curve_id)
        .unwrap()
        .evaluate(0.0)
        .unwrap();
    let end = sketch
        .evaluated_curve(curve_id)
        .unwrap()
        .evaluate(1.0)
        .unwrap();
    assert!((start.u - 0.0).abs() < 1.0e-5 && (start.v - 0.0).abs() < 1.0e-5);
    assert!((end.u - 30.0).abs() < 1.0e-5 && (end.v - 0.0).abs() < 1.0e-5);
}

#[test]
fn control_vertex_spline_recipe_generates_exact_bspline() {
    let mut sketch = SketchDefinition::new();
    let cv_pts = vec![
        point(0.0, 0.0),
        point(5.0, 10.0),
        point(15.0, 10.0),
        point(20.0, 0.0),
    ];
    let recipe = SketchRecipe::ControlVertexSpline {
        control_points: cv_pts,
        degree: 3,
        knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        weights: None,
        closed: false,
    };
    let transaction = sketch.stage(recipe, "CV Spline").expect("stage cv spline");
    sketch
        .commit(transaction, ConfirmationSource::GreenTick)
        .expect("commit cv spline");

    assert_eq!(sketch.active_points().count(), 4);
    assert_eq!(sketch.active_entities().count(), 1);

    let curve_id = sketch.active_entities().next().unwrap().id;
    let eval = sketch.evaluated_curve(curve_id).expect("evaluated spline");

    let EvaluatedCurve2::Bspline {
        control_points,
        degree,
        ..
    } = eval
    else {
        panic!("expected evaluated bspline");
    };
    assert_eq!(degree, 3);
    assert_eq!(control_points.len(), 4);
    assert_eq!(control_points[0], SketchPoint2::new(0.0, 0.0));
    assert_eq!(control_points[1], SketchPoint2::new(5.0, 10.0));
}

#[test]
fn spline_line_and_spline_spline_intersection() {
    let precision = PrecisionPolicy::default();

    let mut sketch1 = SketchDefinition::new();
    let r1 = SketchRecipe::FitPointSpline {
        fit_points: vec![point(0.0, -10.0), point(10.0, 0.0), point(20.0, 10.0)],
        degree: 2,
        closed: false,
    };
    let t1 = sketch1.stage(r1, "S1").unwrap();
    sketch1.commit(t1, ConfirmationSource::GreenTick).unwrap();
    let s1 = sketch1
        .evaluated_curve(sketch1.active_entities().next().unwrap().id)
        .unwrap();

    let line = EvaluatedCurve2::Line {
        start: SketchPoint2::new(0.0, 0.0),
        end: SketchPoint2::new(20.0, 0.0),
    };

    let result = intersect_curves(s1.clone(), line, &precision);
    let pts = result.unique_points();
    assert_eq!(pts.len(), 1);
    assert!((pts[0].point.u - 10.0).abs() < 1.0e-3);
    assert!((pts[0].point.v - 0.0).abs() < 1.0e-3);
}

#[test]
fn tangent_constraint_and_collinear_constraint_solving() {
    let precision = PrecisionPolicy::default();
    let mut sketch = SketchDefinition::new();
    let t1 = sketch
        .stage(
            SketchRecipe::Point {
                position: SketchPoint2::new(0.0, 0.0),
            },
            "P1",
        )
        .unwrap();
    sketch.commit(t1, ConfirmationSource::GreenTick).unwrap();
    let t2 = sketch
        .stage(
            SketchRecipe::Point {
                position: SketchPoint2::new(5.0, 1.0),
            },
            "P2",
        )
        .unwrap();
    sketch.commit(t2, ConfirmationSource::GreenTick).unwrap();
    let t3 = sketch
        .stage(
            SketchRecipe::Point {
                position: SketchPoint2::new(10.0, 2.0),
            },
            "P3",
        )
        .unwrap();
    sketch.commit(t3, ConfirmationSource::GreenTick).unwrap();
    let t4 = sketch
        .stage(
            SketchRecipe::Point {
                position: SketchPoint2::new(15.0, 5.0),
            },
            "P4",
        )
        .unwrap();
    sketch.commit(t4, ConfirmationSource::GreenTick).unwrap();

    let pts: Vec<_> = sketch.active_points().map(|p| p.id).collect();
    let (p1, p2, p3, p4) = (pts[0], pts[1], pts[2], pts[3]);

    let _ = sketch
        .add_constraint(
            SketchConstraintKind::Collinear {
                first: p1,
                second: p2,
                third: p3,
            },
            precision,
        )
        .unwrap();

    let _ = sketch
        .add_constraint(
            SketchConstraintKind::Tangent {
                first_start: p1,
                first_end: p2,
                second_start: p3,
                second_end: p4,
            },
            precision,
        )
        .unwrap();

    let solved = sketch.solve_constraints(precision);
    assert!(solved.is_ok());
}

#[test]
fn bspline_curves_compile_to_planar_profile() {
    let precision = PrecisionPolicy::default();
    let mut sketch = SketchDefinition::new();

    let r1 = SketchRecipe::Line {
        start: point(0.0, 0.0),
        end: point(20.0, 0.0),
    };
    let t1 = sketch.stage(r1, "Bottom Line").unwrap();
    sketch.commit(t1, ConfirmationSource::GreenTick).unwrap();

    let r2 = SketchRecipe::FitPointSpline {
        fit_points: vec![point(20.0, 0.0), point(10.0, 10.0), point(0.0, 0.0)],
        degree: 2,
        closed: false,
    };
    let t2 = sketch.stage(r2, "Top Spline").unwrap();
    sketch.commit(t2, ConfirmationSource::GreenTick).unwrap();

    let inputs = sketch.arrangement_inputs().unwrap();
    assert_eq!(inputs.len(), 2);

    let arrangement = build_arrangement(&inputs, &precision, Default::default());
    assert_eq!(arrangement.cells.len(), 1, "{:?}", arrangement.diagnostics);

    let signature = &arrangement.cells[0].signature;
    let compiled =
        compile_selected_profile(&arrangement, std::slice::from_ref(signature), &precision)
            .unwrap();
    assert_eq!(compiled.profile.regions.len(), 1);
    assert_eq!(compiled.profile.regions[0].outer.curves.len(), 2);

    assert!(
        compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .any(|c| matches!(c, PlanarCurve2::Bspline { .. }))
    );
    assert!(
        compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .any(|c| matches!(c, PlanarCurve2::Line { .. }))
    );
}

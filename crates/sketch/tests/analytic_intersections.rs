use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    CurveDirection, CurveIntersections, EvaluatedCurve2, IntersectionClass, SketchPoint2,
    intersect_curves,
};

fn line(start: (f64, f64), end: (f64, f64)) -> EvaluatedCurve2 {
    EvaluatedCurve2::Line {
        start: SketchPoint2::new(start.0, start.1),
        end: SketchPoint2::new(end.0, end.1),
    }
}

fn circle(center: (f64, f64), radius: f64) -> EvaluatedCurve2 {
    EvaluatedCurve2::Circle {
        center: SketchPoint2::new(center.0, center.1),
        radius,
        direction: CurveDirection::CounterClockwise,
    }
}

fn assert_reversal_symmetric(first: EvaluatedCurve2, second: EvaluatedCurve2) {
    let precision = PrecisionPolicy::default();
    assert_eq!(
        intersect_curves(first.clone(), second.clone(), &precision),
        intersect_curves(second, first, &precision).reversed()
    );
}

#[test]
fn entire_pair_matrix_is_operand_reversal_symmetric() {
    assert_reversal_symmetric(line((-2.0, 0.0), (2.0, 0.0)), line((0.0, -2.0), (0.0, 2.0)));
    assert_reversal_symmetric(line((-2.0, 0.25), (2.0, 0.25)), circle((0.0, 0.0), 1.0));
    assert_reversal_symmetric(circle((0.0, 0.0), 2.0), circle((2.0, 0.0), 2.0));

    let arc = EvaluatedCurve2::CircularArc {
        center: SketchPoint2::new(0.0, 0.0),
        start: SketchPoint2::new(2.0, 0.0),
        end: SketchPoint2::new(-2.0, 0.0),
        direction: CurveDirection::CounterClockwise,
    };
    assert_reversal_symmetric(line((0.0, -3.0), (0.0, 3.0)), arc.clone());
    assert_reversal_symmetric(arc.clone(), circle((1.5, 0.0), 2.0));
    assert_reversal_symmetric(
        arc,
        EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(1.5, 0.0),
            start: SketchPoint2::new(3.5, 0.0),
            end: SketchPoint2::new(-0.5, 0.0),
            direction: CurveDirection::CounterClockwise,
        },
    );
}

#[test]
fn tangent_endpoint_and_overlap_remain_distinct_typed_outcomes() {
    let precision = PrecisionPolicy::default();
    let tangent = intersect_curves(
        line((-2.0, 1.0), (2.0, 1.0)),
        circle((0.0, 0.0), 1.0),
        &precision,
    );
    assert_eq!(tangent.unique_points()[0].class, IntersectionClass::Tangent);
    assert!(tangent.unique_points()[0].is_tangent);

    let endpoint = intersect_curves(
        line((0.0, 0.0), (1.0, 0.0)),
        line((1.0, 0.0), (2.0, 1.0)),
        &precision,
    );
    assert_eq!(
        endpoint.unique_points()[0].class,
        IntersectionClass::EndpointEndpoint
    );

    let overlap = intersect_curves(
        line((0.0, 0.0), (3.0, 0.0)),
        line((1.0, 0.0), (2.0, 0.0)),
        &precision,
    );
    assert!(matches!(overlap, CurveIntersections::Overlap { .. }));
}

#[test]
fn arc_domain_filters_only_the_excluded_carrier_branch() {
    let precision = PrecisionPolicy::default();
    let upper_arc = EvaluatedCurve2::CircularArc {
        center: SketchPoint2::new(0.0, 0.0),
        start: SketchPoint2::new(1.0, 0.0),
        end: SketchPoint2::new(-1.0, 0.0),
        direction: CurveDirection::CounterClockwise,
    };
    let result = intersect_curves(line((0.0, -2.0), (0.0, 2.0)), upper_arc, &precision);
    assert_eq!(result.unique_points().len(), 1);
    assert!(result.unique_points()[0].point.v > 0.0);
}

#[test]
fn near_tangent_resolution_band_is_indeterminate_not_silently_snapped() {
    let precision = PrecisionPolicy::default();
    let exact = intersect_curves(
        line((-2.0, 1.0), (2.0, 1.0)),
        circle((0.0, 0.0), 1.0),
        &precision,
    );
    assert_eq!(exact.unique_points()[0].class, IntersectionClass::Tangent);

    for offset in [-0.25e-6, 0.25e-6] {
        assert!(matches!(
            intersect_curves(
                line((-2.0, 1.0 + offset), (2.0, 1.0 + offset)),
                circle((0.0, 0.0), 1.0),
                &precision,
            ),
            CurveIntersections::Indeterminate { .. }
        ));
    }
    assert!(matches!(
        intersect_curves(
            line((-2.0, 1.0 + 2.0e-6), (2.0, 1.0 + 2.0e-6)),
            circle((0.0, 0.0), 1.0),
            &precision,
        ),
        CurveIntersections::Disjoint
    ));
    assert_eq!(
        intersect_curves(
            line((-2.0, 1.0 - 2.0e-6), (2.0, 1.0 - 2.0e-6)),
            circle((0.0, 0.0), 1.0),
            &precision,
        )
        .unique_points()
        .len(),
        2
    );
}

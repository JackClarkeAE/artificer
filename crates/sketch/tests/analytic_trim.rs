use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    CurveDirection, EvaluatedCurve2, SketchEntityId, SketchPoint2, TrimCurve, TrimError,
    select_trim_span,
};

fn eid(raw: u64) -> SketchEntityId {
    SketchEntityId::new(raw).unwrap()
}

fn line(entity: u64, start: (f64, f64), end: (f64, f64)) -> TrimCurve {
    TrimCurve {
        entity: eid(entity),
        curve: EvaluatedCurve2::Line {
            start: SketchPoint2::new(start.0, start.1),
            end: SketchPoint2::new(end.0, end.1),
        },
    }
}

#[test]
fn enclosed_middle_span_is_removed_without_touching_outer_spans() {
    let precision = PrecisionPolicy::default();
    let result = select_trim_span(
        line(1, (-4.0, 0.0), (4.0, 0.0)),
        &[
            line(2, (-1.0, -2.0), (-1.0, 2.0)),
            line(3, (1.0, -2.0), (1.0, 2.0)),
        ],
        SketchPoint2::new(0.0, 0.0),
        &precision,
        64,
    )
    .unwrap();
    assert_eq!(result.retained.len(), 2);
    assert!(result.removed.start_limit.is_some());
    assert!(result.removed.end_limit.is_some());
    assert!(!result.removed.source_interval.wraps_periodic_seam);
}

#[test]
fn circle_wrap_span_is_exact_and_clicking_a_junction_is_ambiguous() {
    let precision = PrecisionPolicy::default();
    let circle = TrimCurve {
        entity: eid(1),
        curve: EvaluatedCurve2::Circle {
            center: SketchPoint2::new(0.0, 0.0),
            radius: 2.0,
            direction: CurveDirection::CounterClockwise,
        },
    };
    let limits = [line(2, (0.0, -3.0), (0.0, 3.0))];
    let wrap =
        select_trim_span(circle, &limits, SketchPoint2::new(2.0, 0.0), &precision, 64).unwrap();
    assert!(wrap.removed.source_interval.wraps_periodic_seam);
    assert!(matches!(
        wrap.removed.curve,
        EvaluatedCurve2::CircularArc { .. }
    ));

    assert!(matches!(
        select_trim_span(circle, &limits, SketchPoint2::new(0.0, 2.0), &precision, 64),
        Err(TrimError::ClickAtJunction { .. })
    ));
}

#[test]
fn coincident_limit_never_picks_an_arbitrary_span() {
    let precision = PrecisionPolicy::default();
    assert!(matches!(
        select_trim_span(
            line(1, (0.0, 0.0), (4.0, 0.0)),
            &[line(2, (1.0, 0.0), (3.0, 0.0))],
            SketchPoint2::new(2.0, 0.0),
            &precision,
            64
        ),
        Err(TrimError::NoUniqueSpan { .. })
    ));
}

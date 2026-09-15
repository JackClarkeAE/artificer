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
    let wrap = select_trim_span(
        circle.clone(),
        &limits,
        SketchPoint2::new(2.0, 0.0),
        &precision,
        64,
    )
    .unwrap();
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

/// A limit drawn along the target bounds it where their overlap starts and
/// ends. Those two points are not arbitrary — they are exactly what a drafter
/// means by trimming a line against one drawn over it, which is the shape a
/// wedge whose base runs along a rectangle's edge leaves behind. Refusing the
/// whole trim instead made that edge untrimmable.
#[test]
fn a_limit_along_the_target_bounds_it_where_the_overlap_ends() {
    let precision = PrecisionPolicy::default();
    let selection = select_trim_span(
        line(1, (0.0, 0.0), (4.0, 0.0)),
        &[line(2, (1.0, 0.0), (3.0, 0.0))],
        SketchPoint2::new(2.0, 0.0),
        &precision,
        64,
    )
    .expect("the ends of the overlap bound the span");
    assert_eq!(selection.retained.len(), 2);
    assert!((selection.removed.source_interval.start - 0.25).abs() < 1.0e-9);
    assert!((selection.removed.source_interval.end - 0.75).abs() < 1.0e-9);
}

/// A duplicate drawn over the whole target bounds it only at its own ends, so
/// the single span is the whole curve and nothing is retained. Trimming one of
/// two identical lines takes that line, which is what the click asked for.
#[test]
fn a_duplicate_over_the_whole_target_leaves_nothing_of_it() {
    let precision = PrecisionPolicy::default();
    let selection = select_trim_span(
        line(1, (0.0, 0.0), (4.0, 0.0)),
        &[line(2, (0.0, 0.0), (4.0, 0.0))],
        SketchPoint2::new(2.0, 0.0),
        &precision,
        64,
    )
    .expect("the whole curve is one span");
    assert!(selection.retained.is_empty());
    assert!((selection.removed.source_interval.start).abs() < 1.0e-9);
    assert!((selection.removed.source_interval.end - 1.0).abs() < 1.0e-9);
}

//! A curve that very nearly touches another must not take the sketch with it.
//!
//! ADR 0002 lets an algorithm answer "indeterminate" rather than invent
//! topology, and near a tangency that is the honest answer: whether a line
//! misses a circle, touches it, or cuts it cannot be certified while the
//! separation sits inside modelling resolution. The arrangement's response is
//! to drop both curves, which silently costs the user every region those
//! curves bounded.
//!
//! These gates pin that behaviour so it stays *visible*. They are not an
//! argument that the refusal is wrong; they are the record of exactly which
//! configurations lose their regions, so the UI can say so and so a later
//! widening of the certified domain has something to measure itself against.

use artificer_protocol::PrecisionPolicy;
use artificer_sketch::{
    ArrangementDiagnostic, ArrangementInputCurve, ArrangementLimits, CurveDirection,
    EvaluatedCurve2, SketchEntityId, SketchPoint2, build_arrangement,
};

fn point(u: f64, v: f64) -> SketchPoint2 {
    SketchPoint2::new(u, v)
}

/// A circle of radius 4 at the origin, cut by a rectangle whose top edge is the
/// horizontal chord through its centre.
///
/// `half_width` is where the rectangle's vertical sides stand. At exactly 4
/// they are tangent to the circle; a hair either side of that is the case a
/// user creates by dragging a rectangle corner onto a circle's edge without
/// saying, in a relation, that it belongs there.
fn circle_cut_by_rectangle(half_width: f64) -> Vec<ArrangementInputCurve> {
    let mut curves = Vec::new();
    let mut next = 1_u64;
    let mut push = |curve: EvaluatedCurve2, curves: &mut Vec<ArrangementInputCurve>| {
        curves.push(ArrangementInputCurve {
            entity: SketchEntityId::new(next).expect("a positive entity id"),
            curve,
            start_point: None,
            end_point: None,
        });
        next += 1;
    };
    push(
        EvaluatedCurve2::Circle {
            center: point(0.0, 0.0),
            radius: 4.0,
            direction: CurveDirection::CounterClockwise,
        },
        &mut curves,
    );
    let corners = [
        point(-half_width, 0.0),
        point(half_width, 0.0),
        point(half_width, -8.0),
        point(-half_width, -8.0),
    ];
    for index in 0..4 {
        push(
            EvaluatedCurve2::Line {
                start: corners[index],
                end: corners[(index + 1) % 4],
            },
            &mut curves,
        );
    }
    // One untouched circle well clear of the rest, which must survive whatever
    // happens to the pair above.
    push(
        EvaluatedCurve2::Circle {
            center: point(0.0, -16.0),
            radius: 3.0,
            direction: CurveDirection::CounterClockwise,
        },
        &mut curves,
    );
    curves
}

fn cells_at(half_width: f64) -> (usize, usize) {
    let precision = PrecisionPolicy::default();
    let arrangement = build_arrangement(
        &circle_cut_by_rectangle(half_width),
        &precision,
        ArrangementLimits::default(),
    );
    let indeterminate = arrangement
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic,
                ArrangementDiagnostic::IndeterminateIntersection { .. }
            )
        })
        .count();
    (arrangement.cells.len(), indeterminate)
}

/// Exact tangency is certifiable, so the sketch keeps every region: the two
/// halves of the disc and the rest of the rectangle.
#[test]
fn an_exactly_tangent_rectangle_keeps_every_region() {
    let (cells, indeterminate) = cells_at(4.0);
    assert_eq!(
        cells, 4,
        "two half-discs, the rectangle's remainder, and the free circle"
    );
    assert_eq!(indeterminate, 0, "an exact tangency needs no guessing");
}

/// A frank crossing is certifiable too.
#[test]
fn a_frankly_crossing_rectangle_keeps_every_region() {
    for half_width in [3.9, 3.99, 4.01, 4.1] {
        let (cells, indeterminate) = cells_at(half_width);
        assert_eq!(cells, 4, "at half-width {half_width}");
        assert_eq!(indeterminate, 0, "at half-width {half_width}");
    }
}

/// The reported failure, pinned. Inside the resolution band the crossing is
/// refused, both curves are dropped, and every region they bounded goes with
/// them — leaving only the untouched circle.
///
/// This is the behaviour the status line now explains rather than performing
/// in silence. If the certified domain is ever widened so these resolve, this
/// gate is what will fail and say so.
#[test]
fn a_near_tangent_rectangle_loses_its_regions_and_says_why() {
    for half_width in [
        4.0 - 1.0e-6,
        4.0 - 1.0e-7,
        4.0 - 1.0e-8,
        4.0 + 1.0e-8,
        4.0 + 1.0e-7,
    ] {
        let (cells, indeterminate) = cells_at(half_width);
        assert_eq!(
            cells, 1,
            "at half-width {half_width} only the untouched circle survives"
        );
        assert!(
            indeterminate > 0,
            "at half-width {half_width} the loss must be recorded, not silent"
        );
    }
}

/// Whatever happens to the near-tangent pair, geometry that has nothing to do
/// with it keeps its regions. A local refusal is never allowed to become a
/// whole-sketch one.
#[test]
fn an_unrelated_region_survives_a_refused_crossing() {
    let precision = PrecisionPolicy::default();
    let arrangement = build_arrangement(
        &circle_cut_by_rectangle(4.0 - 1.0e-7),
        &precision,
        ArrangementLimits::default(),
    );
    assert!(
        arrangement
            .cell_at_point(point(0.0, -16.0), &precision)
            .is_some(),
        "the free circle is nowhere near the refused pair"
    );
}

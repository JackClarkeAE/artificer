use artificer_protocol::{ArcDirection, PlanarCurve2, Point2, PrecisionPolicy};
use artificer_sketch::{
    ArrangementDiagnostic, ArrangementInputCurve, ArrangementLimits, CurveDirection,
    ProfileCompileError, SketchEntityId, SketchPoint2, SketchPointId, build_arrangement,
    compile_selected_profile,
};

fn eid(raw: u64) -> SketchEntityId {
    SketchEntityId::new(raw).unwrap()
}

fn pid(raw: u64) -> SketchPointId {
    SketchPointId::new(raw).unwrap()
}

fn rectangle(
    entity_base: u64,
    point_base: u64,
    min: (f64, f64),
    max: (f64, f64),
) -> Vec<ArrangementInputCurve> {
    let points = [
        SketchPoint2::new(min.0, min.1),
        SketchPoint2::new(max.0, min.1),
        SketchPoint2::new(max.0, max.1),
        SketchPoint2::new(min.0, max.1),
    ];
    (0..4)
        .map(|index| {
            ArrangementInputCurve::line(
                eid(entity_base + index as u64),
                pid(point_base + index as u64),
                pid(point_base + ((index + 1) % 4) as u64),
                points[index],
                points[(index + 1) % 4],
            )
        })
        .collect()
}

fn planar_endpoints(curve: PlanarCurve2) -> Option<(Point2, Point2)> {
    match curve {
        PlanarCurve2::Line { start, end } | PlanarCurve2::CircularArc { start, end, .. } => {
            Some((start, end))
        }
        PlanarCurve2::Bspline { control_points, .. } => {
            Some((control_points[0], *control_points.last().unwrap()))
        }
        PlanarCurve2::Circle { .. } => None,
    }
}

#[test]
fn rectangle_signature_is_invariant_to_authored_curve_reversal() {
    let precision = PrecisionPolicy::default();
    let forward = rectangle(1, 1, (0.0, 0.0), (4.0, 3.0));
    let reversed: Vec<_> = forward
        .iter()
        .map(|input| ArrangementInputCurve {
            entity: input.entity,
            curve: input.curve.reverse(),
            start_point: input.end_point,
            end_point: input.start_point,
        })
        .collect();
    let first = build_arrangement(&forward, &precision, ArrangementLimits::default());
    let second = build_arrangement(&reversed, &precision, ArrangementLimits::default());
    let mut permuted = forward.clone();
    permuted.rotate_left(2);
    permuted.reverse();
    let third = build_arrangement(&permuted, &precision, ArrangementLimits::default());
    assert_eq!(first.cells.len(), 1);
    assert_eq!(second.cells.len(), 1);
    assert_eq!(third.cells.len(), 1);
    assert_eq!(first.cells[0].signature, second.cells[0].signature);
    assert_eq!(first.cells[0].signature, third.cells[0].signature);
}

#[test]
fn overlapping_rectangles_form_three_selectable_bounded_cells() {
    let precision = PrecisionPolicy::default();
    let mut curves = rectangle(1, 1, (0.0, 0.0), (4.0, 3.0));
    curves.extend(rectangle(10, 10, (2.0, 1.0), (6.0, 4.0)));
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 3, "{:?}", arrangement.diagnostics);
    let area: f64 = arrangement.cells.iter().map(|cell| cell.signed_area).sum();
    assert!((area - 20.0).abs() < 1.0e-9);
}

#[test]
fn capsule_slot_is_one_exact_mixed_line_arc_cell() {
    let precision = PrecisionPolicy::default();
    let points = [
        SketchPoint2::new(-2.0, -1.0),
        SketchPoint2::new(2.0, -1.0),
        SketchPoint2::new(2.0, 1.0),
        SketchPoint2::new(-2.0, 1.0),
    ];
    let curves = [
        ArrangementInputCurve::line(eid(1), pid(1), pid(2), points[0], points[1]),
        ArrangementInputCurve::circular_arc(
            eid(2),
            SketchPoint2::new(2.0, 0.0),
            pid(2),
            pid(3),
            points[1],
            points[2],
            CurveDirection::CounterClockwise,
        ),
        ArrangementInputCurve::line(eid(3), pid(3), pid(4), points[2], points[3]),
        ArrangementInputCurve::circular_arc(
            eid(4),
            SketchPoint2::new(-2.0, 0.0),
            pid(4),
            pid(1),
            points[3],
            points[0],
            CurveDirection::CounterClockwise,
        ),
    ];
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 1, "{:?}", arrangement.diagnostics);
    let compiled = compile_selected_profile(
        &arrangement,
        &[arrangement.cells[0].signature.clone()],
        &precision,
    )
    .unwrap();
    assert_eq!(compiled.profile.curve_count(), 4);
    assert_eq!(
        compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .filter(|curve| matches!(curve, PlanarCurve2::CircularArc { .. }))
            .count(),
        2
    );
}

#[test]
fn semicircle_and_diameter_compile_with_bitwise_connected_authored_endpoints() {
    let precision = PrecisionPolicy::default();
    let right = SketchPoint2::new(2.0, 0.0);
    let left = SketchPoint2::new(-2.0, 0.0);
    let curves = [
        ArrangementInputCurve::circular_arc(
            eid(1),
            SketchPoint2::new(0.0, 0.0),
            pid(1),
            pid(2),
            right,
            left,
            CurveDirection::CounterClockwise,
        ),
        ArrangementInputCurve::line(eid(2), pid(2), pid(1), left, right),
    ];
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 1, "{:?}", arrangement.diagnostics);
    let profile = compile_selected_profile(
        &arrangement,
        &[arrangement.cells[0].signature.clone()],
        &precision,
    )
    .expect("semicircle profile")
    .profile;
    let output = &profile.regions[0].outer.curves;
    assert_eq!(output.len(), 2);
    for index in 0..output.len() {
        let (_, end) = planar_endpoints(output[index].clone()).expect("nonperiodic curve");
        let (next_start, _) = planar_endpoints(output[(index + 1) % output.len()].clone())
            .expect("nonperiodic curve");
        assert_eq!(end.x.to_bits(), next_start.x.to_bits());
        assert_eq!(end.y.to_bits(), next_start.y.to_bits());
    }
}

#[test]
fn nested_circles_compile_as_an_exact_annulus() {
    let precision = PrecisionPolicy::default();
    let curves = [
        ArrangementInputCurve::circle(
            eid(1),
            SketchPoint2::new(0.0, 0.0),
            5.0,
            CurveDirection::CounterClockwise,
        ),
        ArrangementInputCurve::circle(
            eid(2),
            SketchPoint2::new(0.0, 0.0),
            2.0,
            CurveDirection::CounterClockwise,
        ),
    ];
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    let annulus = arrangement
        .cells
        .iter()
        .find(|cell| cell.holes.len() == 1)
        .unwrap();
    let compiled = compile_selected_profile(
        &arrangement,
        std::slice::from_ref(&annulus.signature),
        &precision,
    )
    .unwrap();
    assert_eq!(compiled.profile.regions.len(), 1);
    assert_eq!(compiled.profile.regions[0].holes.len(), 1);
    assert!(matches!(
        compiled.profile.regions[0].outer.curves[0],
        PlanarCurve2::Circle {
            direction: ArcDirection::CounterClockwise,
            ..
        }
    ));
    assert!(matches!(
        compiled.profile.regions[0].holes[0].curves[0],
        PlanarCurve2::Circle {
            direction: ArcDirection::Clockwise,
            ..
        }
    ));
}

#[test]
fn attached_dangling_geometry_is_not_exported_as_part_of_the_cell() {
    let precision = PrecisionPolicy::default();
    let mut curves = rectangle(1, 1, (0.0, 0.0), (4.0, 3.0));
    curves.push(ArrangementInputCurve::line(
        eid(10),
        pid(10),
        pid(11),
        SketchPoint2::new(2.0, 3.0),
        SketchPoint2::new(2.0, 5.0),
    ));
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 1, "{:?}", arrangement.diagnostics);
    let compiled = compile_selected_profile(
        &arrangement,
        &[arrangement.cells[0].signature.clone()],
        &precision,
    )
    .unwrap();
    assert_eq!(compiled.profile.curve_count(), 5);
    assert!(
        compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .all(|curve| {
                !matches!(curve, PlanarCurve2::Line { start, end } if start.y > 3.0 || end.y > 3.0)
            })
    );
}

#[test]
fn point_kissing_loops_each_remain_selectable_but_refuse_a_pinched_union() {
    let precision = PrecisionPolicy::default();
    let mut curves = rectangle(1, 1, (0.0, 0.0), (2.0, 2.0));
    curves.extend(rectangle(10, 10, (2.0, 2.0), (4.0, 4.0)));
    curves.extend(rectangle(20, 20, (8.0, 0.0), (10.0, 2.0)));
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 3, "{:?}", arrangement.diagnostics);
    assert!(
        !arrangement
            .diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, ArrangementDiagnostic::KissingJunction { .. }))
    );
    let lower = arrangement
        .cell_at_point(SketchPoint2::new(1.0, 1.0), &precision)
        .expect("lower kissing square");
    let upper = arrangement
        .cell_at_point(SketchPoint2::new(3.0, 3.0), &precision)
        .expect("upper kissing square");
    assert!(
        compile_selected_profile(
            &arrangement,
            std::slice::from_ref(&lower.signature),
            &precision
        )
        .is_ok()
    );
    assert!(matches!(
        compile_selected_profile(
            &arrangement,
            &[lower.signature.clone(), upper.signature.clone()],
            &precision
        ),
        Err(ProfileCompileError::PinchedBoundary)
    ));
}

#[test]
fn a_polygon_inscribed_in_a_circle_splits_it_into_selectable_segments() {
    let precision = PrecisionPolicy::default();
    // A diamond whose vertices sit exactly on the rim: every vertex is a
    // four-departure junction carrying authored endpoints.
    let vertices = [
        SketchPoint2::new(2.0, 0.0),
        SketchPoint2::new(0.0, 2.0),
        SketchPoint2::new(-2.0, 0.0),
        SketchPoint2::new(0.0, -2.0),
    ];
    let mut curves: Vec<_> = (0..4)
        .map(|index| {
            ArrangementInputCurve::line(
                eid(1 + index as u64),
                pid(1 + index as u64),
                pid(1 + ((index + 1) % 4) as u64),
                vertices[index],
                vertices[(index + 1) % 4],
            )
        })
        .collect();
    curves.push(ArrangementInputCurve::circle(
        eid(10),
        SketchPoint2::new(0.0, 0.0),
        2.0,
        CurveDirection::CounterClockwise,
    ));
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 5, "{:?}", arrangement.diagnostics);
    assert!(
        arrangement.diagnostics.is_empty(),
        "{:?}",
        arrangement.diagnostics
    );
    let inner = arrangement
        .cell_at_point(SketchPoint2::new(0.0, 0.0), &precision)
        .expect("the inscribed diamond");
    let segment = arrangement
        // Inside the rim (radius 1.8) but beyond the diamond edge u + v = 2.
        .cell_at_point(SketchPoint2::new(1.27, 1.27), &precision)
        .expect("one circular segment");
    assert_ne!(inner.signature, segment.signature);
    let compiled = compile_selected_profile(
        &arrangement,
        std::slice::from_ref(&segment.signature),
        &precision,
    )
    .unwrap();
    assert_eq!(compiled.profile.regions.len(), 1);
    assert_eq!(compiled.profile.regions[0].outer.curves.len(), 2);
}

#[test]
fn spokes_from_a_shared_centre_divide_a_square_into_quadrants() {
    let precision = PrecisionPolicy::default();
    let mut curves = rectangle(1, 1, (0.0, 0.0), (4.0, 4.0));
    let centre = SketchPoint2::new(2.0, 2.0);
    let rim = [
        SketchPoint2::new(2.0, 0.0),
        SketchPoint2::new(4.0, 2.0),
        SketchPoint2::new(2.0, 4.0),
        SketchPoint2::new(0.0, 2.0),
    ];
    for (index, end) in rim.into_iter().enumerate() {
        curves.push(ArrangementInputCurve::line(
            eid(10 + index as u64),
            pid(10),
            pid(11 + index as u64),
            centre,
            end,
        ));
    }
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 4, "{:?}", arrangement.diagnostics);
    assert!(
        arrangement.diagnostics.is_empty(),
        "{:?}",
        arrangement.diagnostics
    );
}

#[test]
fn crossing_rectangle_and_circle_form_selectable_exact_mixed_cells() {
    let precision = PrecisionPolicy::default();
    let mut curves = rectangle(1, 1, (-2.0, -1.0), (2.0, 1.0));
    curves.push(ArrangementInputCurve::circle(
        eid(10),
        SketchPoint2::new(0.0, 0.0),
        1.5,
        CurveDirection::CounterClockwise,
    ));
    let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
    assert_eq!(arrangement.cells.len(), 5, "{:?}", arrangement.diagnostics);
    assert!(
        arrangement
            .cell_at_point(SketchPoint2::new(0.0, 0.0), &precision)
            .is_some()
    );
    let selected: Vec<_> = arrangement
        .cells
        .iter()
        .map(|cell| cell.signature.clone())
        .collect();
    let compiled = compile_selected_profile(&arrangement, &selected, &precision).unwrap();
    assert_eq!(compiled.profile.regions.len(), 1);
    assert!(
        compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .any(|curve| matches!(curve, PlanarCurve2::CircularArc { .. }))
    );
    assert!(
        compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .any(|curve| matches!(curve, PlanarCurve2::Line { .. }))
    );
}

use std::collections::BTreeMap;

use artificer_protocol::{ArcDirection, PlanarCurve2, Point2, PrecisionPolicy};
use artificer_sketch::{
    ArrangementInputCurve, ArrangementLimits, CurveDirection, EvaluatedCurve2, SketchEntityId,
    SketchPoint2, SketchPointId, SketchVector2, build_arrangement, compile_selected_profile,
};

fn eid(raw: u64) -> SketchEntityId {
    SketchEntityId::new(raw).expect("non-zero entity ID")
}

fn pid(raw: u64) -> SketchPointId {
    SketchPointId::new(raw).expect("non-zero point ID")
}

fn capsule_with_hole() -> Vec<ArrangementInputCurve> {
    let lower_left = SketchPoint2::new(-2.0, -1.0);
    let lower_right = SketchPoint2::new(2.0, -1.0);
    let upper_right = SketchPoint2::new(2.0, 1.0);
    let upper_left = SketchPoint2::new(-2.0, 1.0);
    vec![
        ArrangementInputCurve::line(eid(1), pid(1), pid(2), lower_left, lower_right),
        ArrangementInputCurve::circular_arc(
            eid(2),
            SketchPoint2::new(2.0, 0.0),
            pid(2),
            pid(3),
            lower_right,
            upper_right,
            CurveDirection::CounterClockwise,
        ),
        ArrangementInputCurve::line(eid(3), pid(3), pid(4), upper_right, upper_left),
        ArrangementInputCurve::circular_arc(
            eid(4),
            SketchPoint2::new(-2.0, 0.0),
            pid(4),
            pid(1),
            upper_left,
            lower_left,
            CurveDirection::CounterClockwise,
        ),
        ArrangementInputCurve::circle(
            eid(5),
            SketchPoint2::new(0.0, 0.0),
            0.5,
            CurveDirection::CounterClockwise,
        ),
    ]
}

#[derive(Clone, Copy, Debug)]
struct Similarity2 {
    translation: SketchVector2,
    angle: f64,
    scale: f64,
    reflected: bool,
}

impl Similarity2 {
    fn apply_point(self, point: SketchPoint2) -> SketchPoint2 {
        let reflected_u = if self.reflected { -point.u } else { point.u };
        let cosine = self.angle.cos();
        let sine = self.angle.sin();
        SketchPoint2::new(
            self.scale * cosine.mul_add(reflected_u, -(sine * point.v)) + self.translation.u,
            self.scale * sine.mul_add(reflected_u, cosine * point.v) + self.translation.v,
        )
    }

    fn direction(self, direction: CurveDirection) -> CurveDirection {
        if self.reflected {
            match direction {
                CurveDirection::CounterClockwise => CurveDirection::Clockwise,
                CurveDirection::Clockwise => CurveDirection::CounterClockwise,
            }
        } else {
            direction
        }
    }

    fn protocol_direction(self, direction: ArcDirection) -> ArcDirection {
        if self.reflected {
            match direction {
                ArcDirection::CounterClockwise => ArcDirection::Clockwise,
                ArcDirection::Clockwise => ArcDirection::CounterClockwise,
            }
        } else {
            direction
        }
    }

    fn apply_curve(self, curve: EvaluatedCurve2) -> EvaluatedCurve2 {
        match curve {
            EvaluatedCurve2::Line { start, end } => EvaluatedCurve2::Line {
                start: self.apply_point(start),
                end: self.apply_point(end),
            },
            EvaluatedCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => EvaluatedCurve2::CircularArc {
                center: self.apply_point(center),
                start: self.apply_point(start),
                end: self.apply_point(end),
                direction: self.direction(direction),
            },
            EvaluatedCurve2::Circle {
                center,
                radius,
                direction,
            } => EvaluatedCurve2::Circle {
                center: self.apply_point(center),
                radius: radius * self.scale,
                direction: self.direction(direction),
            },
            EvaluatedCurve2::Bspline {
                control_points,
                degree,
                knots,
                weights,
            } => EvaluatedCurve2::Bspline {
                control_points: control_points.into_iter().map(|p| self.apply_point(p)).collect(),
                degree,
                knots,
                weights,
            },
        }
    }

    fn apply_input(self, input: ArrangementInputCurve) -> ArrangementInputCurve {
        ArrangementInputCurve {
            entity: input.entity,
            curve: self.apply_curve(input.curve),
            start_point: input.start_point,
            end_point: input.end_point,
        }
    }

    fn apply_protocol_point(self, point: Point2) -> Point2 {
        let transformed = self.apply_point(SketchPoint2::new(point.x, point.y));
        Point2::new(transformed.u, transformed.v)
    }

    fn apply_planar_curve(self, curve: PlanarCurve2) -> PlanarCurve2 {
        match curve {
            PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
                start: self.apply_protocol_point(start),
                end: self.apply_protocol_point(end),
            },
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => PlanarCurve2::CircularArc {
                center: self.apply_protocol_point(center),
                start: self.apply_protocol_point(start),
                end: self.apply_protocol_point(end),
                direction: self.protocol_direction(direction),
            },
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            } => PlanarCurve2::Circle {
                center: self.apply_protocol_point(center),
                radius: radius * self.scale,
                direction: self.protocol_direction(direction),
            },
            PlanarCurve2::Bspline {
                control_points,
                degree,
                knots,
                weights,
            } => PlanarCurve2::Bspline {
                control_points: control_points
                    .into_iter()
                    .map(|p| self.apply_protocol_point(p))
                    .collect(),
                degree,
                knots,
                weights,
            },
        }
    }
}

fn near_point(first: Point2, second: Point2, tolerance: f64) -> bool {
    (first.x - second.x).hypot(first.y - second.y) <= tolerance
}

fn reverse_direction(direction: ArcDirection) -> ArcDirection {
    match direction {
        ArcDirection::CounterClockwise => ArcDirection::Clockwise,
        ArcDirection::Clockwise => ArcDirection::CounterClockwise,
    }
}

fn protocol_curve_equivalent(first: PlanarCurve2, second: PlanarCurve2, tolerance: f64) -> bool {
    match (first, second) {
        (
            PlanarCurve2::Line {
                start: first_start,
                end: first_end,
            },
            PlanarCurve2::Line {
                start: second_start,
                end: second_end,
            },
        ) => {
            (near_point(first_start, second_start, tolerance)
                && near_point(first_end, second_end, tolerance))
                || (near_point(first_start, second_end, tolerance)
                    && near_point(first_end, second_start, tolerance))
        }
        (
            PlanarCurve2::CircularArc {
                center: first_center,
                start: first_start,
                end: first_end,
                direction: first_direction,
            },
            PlanarCurve2::CircularArc {
                center: second_center,
                start: second_start,
                end: second_end,
                direction: second_direction,
            },
        ) => {
            near_point(first_center, second_center, tolerance)
                && ((near_point(first_start, second_start, tolerance)
                    && near_point(first_end, second_end, tolerance)
                    && first_direction == second_direction)
                    || (near_point(first_start, second_end, tolerance)
                        && near_point(first_end, second_start, tolerance)
                        && first_direction == reverse_direction(second_direction)))
        }
        (
            PlanarCurve2::Circle {
                center: first_center,
                radius: first_radius,
                ..
            },
            PlanarCurve2::Circle {
                center: second_center,
                radius: second_radius,
                ..
            },
        ) => {
            near_point(first_center, second_center, tolerance)
                && (first_radius - second_radius).abs() <= tolerance
        }
        _ => false,
    }
}

fn profile_curves(profile: &artificer_protocol::PlanarProfile2) -> Vec<PlanarCurve2> {
    profile
        .regions
        .iter()
        .flat_map(|region| {
            std::iter::once(&region.outer)
                .chain(region.holes.iter())
                .flat_map(|profile_loop| profile_loop.curves.iter().cloned())
        })
        .collect()
}

#[test]
fn arrangement_and_profile_are_similarity_equivariant_for_a_deterministic_transform_matrix() {
    let precision = PrecisionPolicy::default();
    let source = capsule_with_hole();
    let baseline = build_arrangement(&source, &precision, ArrangementLimits::default());
    assert!(
        baseline.diagnostics.is_empty(),
        "{:?}",
        baseline.diagnostics
    );
    let annulus = baseline
        .cells
        .iter()
        .find(|cell| cell.holes.len() == 1)
        .expect("capsule annulus");
    let baseline_profile = compile_selected_profile(
        &baseline,
        std::slice::from_ref(&annulus.signature),
        &precision,
    )
    .expect("baseline profile")
    .profile;

    let cases = [
        Similarity2 {
            translation: SketchVector2::new(13.0, -7.0),
            angle: 0.0,
            scale: 1.0,
            reflected: false,
        },
        Similarity2 {
            translation: SketchVector2::new(0.0, 0.0),
            angle: std::f64::consts::FRAC_PI_6,
            scale: 1.0,
            reflected: false,
        },
        Similarity2 {
            translation: SketchVector2::new(-2.5, 4.25),
            angle: -std::f64::consts::FRAC_PI_3,
            scale: 3.5,
            reflected: false,
        },
        Similarity2 {
            translation: SketchVector2::new(0.125, -0.375),
            angle: std::f64::consts::FRAC_PI_4,
            scale: 0.125,
            reflected: false,
        },
        Similarity2 {
            translation: SketchVector2::new(0.0, 0.0),
            angle: 0.0,
            scale: 1.0,
            reflected: true,
        },
        Similarity2 {
            translation: SketchVector2::new(40.0, -90.0),
            angle: std::f64::consts::PI * 0.37,
            scale: 8.0,
            reflected: true,
        },
    ];

    for transform in cases {
        let transformed_inputs = source
            .iter()
            .cloned()
            .map(|input| transform.apply_input(input))
            .collect::<Vec<_>>();
        let transformed = build_arrangement(
            &transformed_inputs,
            &precision,
            ArrangementLimits::default(),
        );
        assert!(
            transformed.diagnostics.is_empty(),
            "{transform:?}: {:?}",
            transformed.diagnostics
        );
        assert_eq!(
            transformed.cells.len(),
            baseline.cells.len(),
            "{transform:?}"
        );
        let baseline_cells = baseline
            .cells
            .iter()
            .map(|cell| (&cell.signature, cell))
            .collect::<BTreeMap<_, _>>();
        for cell in &transformed.cells {
            let original = baseline_cells
                .get(&cell.signature)
                .unwrap_or_else(|| panic!("{transform:?}: stable region signature changed"));
            assert!(
                (cell.signed_area - original.signed_area * transform.scale.powi(2)).abs()
                    <= 1.0e-8 * transform.scale.powi(2).max(1.0),
                "{transform:?}: area equivariance failed"
            );
        }

        let transformed_junctions = transformed
            .junctions
            .iter()
            .map(|junction| (&junction.key, junction.point))
            .collect::<BTreeMap<_, _>>();
        for junction in &baseline.junctions {
            let actual = transformed_junctions
                .get(&junction.key)
                .unwrap_or_else(|| panic!("{transform:?}: stable junction key changed"));
            assert!(
                actual.distance(transform.apply_point(junction.point))
                    <= 1.0e-8 * transform.scale.max(1.0),
                "{transform:?}: junction position is not equivariant"
            );
        }

        let transformed_fragments = transformed
            .fragments
            .iter()
            .map(|fragment| (&fragment.key, fragment.curve.clone()))
            .collect::<BTreeMap<_, _>>();
        for fragment in &baseline.fragments {
            let actual = transformed_fragments
                .get(&fragment.key)
                .unwrap_or_else(|| panic!("{transform:?}: stable fragment key changed"));
            let expected = transform.apply_curve(fragment.curve.clone());
            for parameter in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let expected_point = expected
                    .evaluate(if expected.is_periodic() && parameter == 1.0 {
                        0.0
                    } else {
                        parameter
                    })
                    .expect("expected fragment point");
                let actual_point = actual
                    .evaluate(if actual.is_periodic() && parameter == 1.0 {
                        0.0
                    } else {
                        parameter
                    })
                    .expect("actual fragment point");
                assert!(
                    expected_point.distance(actual_point) <= 1.0e-8 * transform.scale.max(1.0),
                    "{transform:?}: fragment geometry is not equivariant"
                );
            }
        }

        let transformed_annulus = transformed
            .cell(&annulus.signature)
            .expect("stable annulus signature");
        assert_eq!(transformed_annulus.holes.len(), 1);
        let transformed_profile = compile_selected_profile(
            &transformed,
            std::slice::from_ref(&annulus.signature),
            &precision,
        )
        .expect("transformed profile")
        .profile;
        assert_eq!(
            transformed_profile.regions.len(),
            baseline_profile.regions.len()
        );
        assert_eq!(
            transformed_profile.curve_count(),
            baseline_profile.curve_count()
        );
        assert_eq!(
            transformed_profile.regions[0].holes.len(),
            baseline_profile.regions[0].holes.len()
        );
        let expected_curves = profile_curves(&baseline_profile)
            .into_iter()
            .map(|curve| transform.apply_planar_curve(curve))
            .collect::<Vec<_>>();
        let mut actual_curves = profile_curves(&transformed_profile);
        let tolerance = 1.0e-8 * transform.scale.max(1.0);
        for expected in expected_curves {
            let index = actual_curves
                .iter()
                .position(|actual| protocol_curve_equivalent(expected.clone(), actual.clone(), tolerance))
                .unwrap_or_else(|| panic!("{transform:?}: profile curve was not equivariant"));
            actual_curves.swap_remove(index);
        }
        assert!(
            actual_curves.is_empty(),
            "{transform:?}: extra profile curves"
        );
    }
}

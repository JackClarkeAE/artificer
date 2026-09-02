//! The first loft rung: a planar profile lofted to its own offset section.
//! Every wall is a plane or a cone, and the volumes are pinned to closed
//! forms — frustum arithmetic and Steiner's offset-area formula.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, ValidationProfile, Vector3,
};

fn frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

fn polygon(points: &[(f64, f64)]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: (0..points.len())
            .map(|index| {
                let (x, y) = points[index];
                let (nx, ny) = points[(index + 1) % points.len()];
                PlanarCurve2::Line {
                    start: Point2::new(x, y),
                    end: Point2::new(nx, ny),
                }
            })
            .collect(),
    }
}

fn square(side: f64) -> PlanarLoop2 {
    polygon(&[(0.0, 0.0), (side, 0.0), (side, side), (0.0, side)])
}

fn circle(center: (f64, f64), radius: f64) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(center.0, center.1),
            radius,
            direction: ArcDirection::CounterClockwise,
        }],
    }
}

fn region(outer: PlanarLoop2, holes: Vec<PlanarLoop2>) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 { outer, holes }],
    }
}

fn loft(profile: PlanarProfile2, distance: f64, offset: f64) -> Result<Snapshot, String> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("loft"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::LoftPlanarProfileOffset {
            frame: frame(),
            profile,
            distance,
            offset,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .map(|outcome| {
            assert!(
                outcome.report.warnings.is_empty(),
                "an offset loft is exact: {:?}",
                outcome.report.warnings
            );
            outcome.snapshot
        })
        .map_err(|error| {
            error
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(",")
        })
}

fn assert_volume(snapshot: &Snapshot, expected: f64) {
    assert!(NativeKernel::validate(snapshot, ValidationProfile::Solid).valid);
    let volume = snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
}

fn frustum(base_area: f64, top_area: f64, height: f64) -> f64 {
    height / 3.0 * (base_area + top_area + (base_area * top_area).sqrt())
}

#[test]
fn a_square_drafted_inward_is_a_pyramid_frustum_of_six_planes() {
    let solid = loft(region(square(20.0), vec![]), 10.0, -2.0).expect("frustum");
    assert_eq!(solid.counts().faces, 6);
    assert_volume(&solid, frustum(400.0, 256.0, 10.0));
    let scene = NativeKernel::debug_scene(&solid);
    // The top cap sits at the full height and is the 16 mm square.
    let top_x = scene
        .triangles
        .iter()
        .filter(|triangle| {
            triangle
                .vertices
                .iter()
                .all(|vertex| (vertex.z - 10.0).abs() < 1.0e-9)
        })
        .flat_map(|triangle| triangle.vertices.iter().map(|vertex| vertex.x))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), x| {
            (min.min(x), max.max(x))
        });
    assert!((top_x.0 - 2.0).abs() < 1.0e-9 && (top_x.1 - 18.0).abs() < 1.0e-9);
}

#[test]
fn a_circle_drafted_outward_is_a_cone_frustum_of_two_half_cones() {
    let solid = loft(region(circle((0.0, 0.0), 10.0), vec![]), 10.0, 3.0).expect("cone");
    assert_eq!(solid.counts().faces, 4, "two caps and two half-cones");
    assert_volume(&solid, PI * 10.0 / 3.0 * (100.0 + 130.0 + 169.0));
    // The rim at the top is the wider circle.
    let scene = NativeKernel::debug_scene(&solid);
    let top_radius = scene
        .edges
        .iter()
        .flat_map(|edge| edge.endpoints)
        .filter(|point| (point.z - 10.0).abs() < 1.0e-9)
        .map(|point| point.x.hypot(point.y))
        .fold(0.0_f64, f64::max);
    assert!((top_radius - 13.0).abs() < 1.0e-9, "{top_radius}");
}

#[test]
fn a_stadium_keeps_its_tangent_arcs_and_matches_steiners_offset_area() {
    // A 10 mm long slot of radius 4: two straight rails tangent to two
    // semicircular caps. Inward offset by 1 over a height of 6.
    let stadium = PlanarLoop2 {
        curves: vec![
            PlanarCurve2::Line {
                start: Point2::new(-5.0, -4.0),
                end: Point2::new(5.0, -4.0),
            },
            PlanarCurve2::CircularArc {
                center: Point2::new(5.0, 0.0),
                start: Point2::new(5.0, -4.0),
                end: Point2::new(5.0, 4.0),
                direction: ArcDirection::CounterClockwise,
            },
            PlanarCurve2::Line {
                start: Point2::new(5.0, 4.0),
                end: Point2::new(-5.0, 4.0),
            },
            PlanarCurve2::CircularArc {
                center: Point2::new(-5.0, 0.0),
                start: Point2::new(-5.0, 4.0),
                end: Point2::new(-5.0, -4.0),
                direction: ArcDirection::CounterClockwise,
            },
        ],
    };
    let area = 80.0 + PI * 16.0;
    let perimeter = 20.0 + 8.0 * PI;
    let (height, inset) = (6.0, 1.0);
    let solid = loft(region(stadium, vec![]), height, -inset).expect("drafted slot");
    assert_eq!(solid.counts().faces, 6, "two caps, two planes, two cones");
    // Steiner: the inward offset by s has area A - P s + pi s^2, integrated
    // over a linear draft.
    let expected =
        height * area - perimeter * height * inset / 2.0 + PI * height * inset * inset / 3.0;
    assert_volume(&solid, expected);
}

#[test]
fn a_hole_shrinks_while_the_outer_boundary_grows() {
    let solid = loft(
        region(square(30.0), vec![circle((15.0, 15.0), 5.0)]),
        10.0,
        2.0,
    )
    .expect("drafted plate with a drafted hole");
    assert_eq!(
        solid.counts().faces,
        8,
        "two caps, four planes, two half-cones"
    );
    let outer = frustum(900.0, 34.0 * 34.0, 10.0);
    let hole = PI * 10.0 / 3.0 * (25.0 + 15.0 + 9.0);
    assert_volume(&solid, outer - hole);
}

#[test]
fn no_draft_is_exactly_the_straight_extrusion() {
    let drafted = loft(region(circle((0.0, 0.0), 10.0), vec![]), 10.0, 0.0).expect("straight");
    assert_eq!(drafted.counts().faces, 4);
    assert_volume(&drafted, PI * 100.0 * 10.0);
}

#[test]
fn infeasible_sections_and_arc_corners_are_refused_by_name() {
    let collapsed = loft(region(square(20.0), vec![]), 10.0, -11.0).expect_err("no section left");
    assert!(
        collapsed.contains("LOFT_SECTION_COLLAPSES")
            || collapsed.contains("LOFT_SECTION_SELF_INTERSECTS"),
        "{collapsed}"
    );

    // A D: a straight edge meeting a semicircle at right angles. The
    // drafted walls would meet in a conic.
    let d_shape = PlanarLoop2 {
        curves: vec![
            PlanarCurve2::Line {
                start: Point2::new(0.0, 5.0),
                end: Point2::new(0.0, -5.0),
            },
            PlanarCurve2::CircularArc {
                center: Point2::new(0.0, 0.0),
                start: Point2::new(0.0, -5.0),
                end: Point2::new(0.0, 5.0),
                direction: ArcDirection::CounterClockwise,
            },
        ],
    };
    let refused = loft(region(d_shape, vec![]), 10.0, -1.0).expect_err("conic corner");
    assert!(refused.contains("LOFT_CORNER_NOT_TANGENT"), "{refused}");
}

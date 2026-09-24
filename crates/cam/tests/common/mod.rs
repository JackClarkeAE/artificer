//! Fixture bodies the CAM tests share, each built through the public kernel
//! protocol so the tests exercise what a document would hold.

#![allow(dead_code)]

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EntityRef,
    ExecuteRequest, FaceExtrusionOperation, KernelCommand, LoftOperation, LoftSection, PlanarAxis2,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, RevolveAngle, Vector3,
};

pub fn execute(snapshot: &Snapshot, label: &str, command: KernelCommand) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
        .snapshot
}

pub fn try_boolean(
    target: &Snapshot,
    tool: &Snapshot,
    operation: BooleanOperation,
    label: &str,
) -> Option<Snapshot> {
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation,
    };
    NativeKernel::execute_boolean(target, tool, &request, &CancellationToken::new())
        .ok()
        .map(|outcome| outcome.snapshot)
}

pub fn polygon(vertices: &[(f64, f64)]) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(
                &vertices
                    .iter()
                    .map(|(x, y)| Point2::new(*x, *y))
                    .collect::<Vec<_>>(),
            ),
            holes: vec![],
        }],
    }
}

/// A rectangle with rounded corners, centred on `centre`, counter-clockwise.
pub fn rounded_rectangle(centre: (f64, f64), width: f64, height: f64, radius: f64) -> PlanarLoop2 {
    let (cx, cy) = centre;
    let (hw, hh) = (width / 2.0, height / 2.0);
    let p = |x: f64, y: f64| Point2::new(cx + x, cy + y);
    let arc = |center: Point2, start: Point2, end: Point2| PlanarCurve2::CircularArc {
        center,
        start,
        end,
        direction: ArcDirection::CounterClockwise,
    };
    let line = |start: Point2, end: Point2| PlanarCurve2::Line { start, end };
    PlanarLoop2 {
        curves: vec![
            line(p(-hw + radius, -hh), p(hw - radius, -hh)),
            arc(
                p(hw - radius, -hh + radius),
                p(hw - radius, -hh),
                p(hw, -hh + radius),
            ),
            line(p(hw, -hh + radius), p(hw, hh - radius)),
            arc(
                p(hw - radius, hh - radius),
                p(hw, hh - radius),
                p(hw - radius, hh),
            ),
            line(p(hw - radius, hh), p(-hw + radius, hh)),
            arc(
                p(-hw + radius, hh - radius),
                p(-hw + radius, hh),
                p(-hw, hh - radius),
            ),
            line(p(-hw, hh - radius), p(-hw, -hh + radius)),
            arc(
                p(-hw + radius, -hh + radius),
                p(-hw, -hh + radius),
                p(-hw + radius, -hh),
            ),
        ],
    }
}

pub fn xy_frame(origin: Point3) -> PlanarFrame3 {
    PlanarFrame3::new(
        origin,
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

/// The XZ plane with `u` radial and `v` along the axis: revolving about the
/// frame's `v` stands the part along world `Z`.
pub fn revolve_frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    )
}

pub fn revolve(profile: PlanarProfile2, label: &str) -> Snapshot {
    execute(
        &NativeKernel::empty(),
        label,
        KernelCommand::RevolvePlanarProfile {
            frame: revolve_frame(),
            profile,
            axis: PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0)),
            angle: RevolveAngle::FullTurn,
            operation: Default::default(),
        },
    )
}

/// The face of `snapshot` whose outward normal is `+Z` at `height`.
pub fn top_face(snapshot: &Snapshot, height: f64) -> EntityRef {
    NativeKernel::describe_faces(snapshot)
        .values()
        .find(|face| face.normal.z > 0.999 && (face.centre.z - height).abs() < 1.0e-6)
        .map(|face| face.face)
        .expect("the body should expose a face at that height")
}

/// The canonical workbench cuboid: 2 × 3 × 4 mm at the origin.
pub fn cuboid() -> Snapshot {
    execute(
        &NativeKernel::empty(),
        "cuboid",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 2.0,
            size_y: 3.0,
            size_z: 4.0,
        },
    )
}

/// The stepped shaft of ADR 0057's lathe gate: Ø40 at the chuck end, Ø30
/// and Ø20 steps towards the free end, a 4 mm × 2 mm groove in the Ø30
/// step and a 2 mm chamfer at the free end. 75 mm long, along world `Z`.
pub fn stepped_shaft() -> Snapshot {
    revolve(
        polygon(&[
            (0.0, 0.0),
            (20.0, 0.0),
            (20.0, 30.0),
            (15.0, 30.0),
            (15.0, 43.0),
            (13.0, 43.0),
            (13.0, 47.0),
            (15.0, 47.0),
            (15.0, 55.0),
            (10.0, 55.0),
            (10.0, 73.0),
            (8.0, 75.0),
            (0.0, 75.0),
        ]),
        "stepped-shaft",
    )
}

/// The volume of [`stepped_shaft`] in closed form.
pub fn stepped_shaft_volume() -> f64 {
    let pi = std::f64::consts::PI;
    let cylinder = |r: f64, h: f64| pi * r * r * h;
    // A frustum from r = 10 to r = 8 over 2 mm.
    let frustum = pi * 2.0 / 3.0 * (10.0_f64.powi(2) + 10.0 * 8.0 + 8.0_f64.powi(2));
    cylinder(20.0, 30.0) + cylinder(15.0, 25.0) - cylinder(15.0, 4.0)
        + cylinder(13.0, 4.0)
        + cylinder(10.0, 18.0)
        + frustum
}

/// A tube: Ø50 outside, Ø30 bore, 40 mm long.
pub fn tube() -> Snapshot {
    revolve(
        polygon(&[(15.0, 0.0), (25.0, 0.0), (25.0, 40.0), (15.0, 40.0)]),
        "tube",
    )
}

/// A ball of radius 10.
pub fn ball() -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, -10.0),
                        end: Point2::new(0.0, 10.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, 0.0),
                        start: Point2::new(0.0, 10.0),
                        end: Point2::new(0.0, -10.0),
                        direction: ArcDirection::Clockwise,
                    },
                ],
            },
            holes: vec![],
        }],
    };
    revolve(profile, "ball")
}

pub const PLATE: (f64, f64, f64) = (80.0, 50.0, 12.0);

/// ADR 0057's mill gate: an 80 × 50 × 12 plate with a rounded outline, a
/// 30 × 20 pocket with 5 mm corners 6 mm deep, a 16 × 12 pocket with 2 mm
/// corners 4 mm deep (which forces a smaller tool), and a Ø6 through hole.
pub fn pocketed_plate() -> Snapshot {
    let (width, height, thickness) = PLATE;
    let plate = execute(
        &NativeKernel::empty(),
        "plate",
        KernelCommand::ExtrudePlanarProfile {
            frame: xy_frame(Point3::new(0.0, 0.0, 0.0)),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: rounded_rectangle((width / 2.0, height / 2.0), width, height, 8.0),
                    holes: vec![],
                }],
            },
            distance: thickness,
        },
    );
    let top = top_face(&plate, thickness);
    let pocket_1 = execute(
        &plate,
        "pocket-1",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top,
            frame: xy_frame(Point3::new(0.0, 0.0, thickness)),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: rounded_rectangle((22.0, 25.0), 30.0, 20.0, 5.0),
                    holes: vec![],
                }],
            },
            distance: 6.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );
    let top = top_face(&pocket_1, thickness);
    let pocket_2 = execute(
        &pocket_1,
        "pocket-2",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top,
            frame: xy_frame(Point3::new(0.0, 0.0, thickness)),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: rounded_rectangle((58.0, 25.0), 16.0, 12.0, 2.0),
                    holes: vec![],
                }],
            },
            distance: 4.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );
    let top = top_face(&pocket_2, thickness);
    execute(
        &pocket_2,
        "through-hole",
        KernelCommand::DrillHole {
            target_face: top,
            frame: xy_frame(Point3::new(0.0, 0.0, thickness)),
            center: Point2::new(40.0, 8.0),
            diameter: 6.0,
            depth: thickness,
        },
    )
}

/// A loft from a square to a circle: ruled walls, which no lathe or 2.5D
/// mill makes.
pub fn square_to_circle_loft() -> Snapshot {
    let square = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&[
                Point2::new(-10.0, -10.0),
                Point2::new(10.0, -10.0),
                Point2::new(10.0, 10.0),
                Point2::new(-10.0, 10.0),
            ]),
            holes: vec![],
        }],
    };
    let circle = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 8.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    execute(
        &NativeKernel::empty(),
        "loft",
        KernelCommand::LoftPlanarSections {
            sections: vec![
                LoftSection {
                    frame: xy_frame(Point3::new(0.0, 0.0, 0.0)),
                    profile: square,
                },
                LoftSection {
                    frame: xy_frame(Point3::new(0.0, 0.0, 20.0)),
                    profile: circle,
                },
            ],
            operation: LoftOperation::New,
        },
    )
}

/// A cylinder along `Z` with a Ø4 hole drilled through it radially along
/// `X`, when the Boolean ladder can make it.
pub fn cross_drilled_cylinder() -> Option<Snapshot> {
    let cylinder = revolve(
        polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 40.0), (0.0, 40.0)]),
        "cross-cylinder",
    );
    let drill = execute(
        &NativeKernel::empty(),
        "cross-drill",
        KernelCommand::RevolvePlanarProfile {
            // `u` along Y and `v` along X: the revolve axis is world X.
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 20.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
            ),
            profile: polygon(&[(0.0, -15.0), (2.0, -15.0), (2.0, 15.0), (0.0, 15.0)]),
            axis: PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0)),
            angle: RevolveAngle::FullTurn,
            operation: Default::default(),
        },
    );
    try_boolean(
        &cylinder,
        &drill,
        BooleanOperation::Difference,
        "cross-drilled",
    )
}

/// A cylinder along `Z` with one flat milled on its side, when the Boolean
/// ladder can make it.
pub fn flatted_cylinder() -> Option<Snapshot> {
    let cylinder = revolve(
        polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 40.0), (0.0, 40.0)]),
        "flat-cylinder",
    );
    let block = execute(
        &NativeKernel::empty(),
        "flat-block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(7.0, -20.0, -5.0),
            size_x: 10.0,
            size_y: 40.0,
            size_z: 50.0,
        },
    );
    try_boolean(&cylinder, &block, BooleanOperation::Difference, "flat")
}

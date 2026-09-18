//! ADR 0044's second answer, as the kernel builds it: a finish cut beside the
//! corner rather than into it.
//!
//! Every expectation here is a closed form derived from inclusion and
//! exclusion, not read off the kernel. A bevel of one edge removes a prism of
//! section `½d²` along it. Two bevels off one corner claim `d³/3` of it twice
//! over; three claim `d³/4` three times. So `n` of them off one corner of a
//! cube of side `L` leave
//!
//! - one:   `L³ − ½d²L`
//! - two:   `L³ − (d²L − d³/3)`
//! - three: `L³ − (3·½d²L − d³ + d³/4)`
//!
//! and those are what a body bevelled one edge at a time must measure.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, KernelCommand, Point3,
    PrecisionPolicy, RequestId,
};

const SIDE: f64 = 10.0;

fn run(input: &Snapshot, command: KernelCommand, label: &str) -> Result<Snapshot, String> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
        .map_err(|error| format!("{error}"))
}

fn cube() -> Snapshot {
    run(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIDE,
            size_y: SIDE,
            size_z: SIDE,
        },
        "cube",
    )
    .expect("a cube")
}

/// The edge of the origin corner running along one axis.
fn origin_edge(snapshot: &Snapshot, axis: usize) -> EntityRef {
    let at = |point: Point3, which: usize| match which {
        0 => point.x,
        1 => point.y,
        _ => point.z,
    };
    NativeKernel::debug_scene(snapshot)
        .edges
        .iter()
        .filter(|edge| !edge.is_smooth)
        .find(|edge| {
            let others = [0, 1, 2].into_iter().filter(|other| *other != axis);
            let runs = (at(edge.endpoints[1], axis) - at(edge.endpoints[0], axis)).abs() > 1.0e-9;
            let flat = others.clone().all(|other| {
                (at(edge.endpoints[1], other) - at(edge.endpoints[0], other)).abs() < 1.0e-9
            });
            let origin = others
                .clone()
                .all(|other| at(edge.endpoints[0], other).abs() < 1.0e-9);
            runs && flat && origin
        })
        .map(|edge| edge.source_edge)
        .unwrap_or_else(|| panic!("the origin corner has an edge along axis {axis}"))
}

fn bevel_apart(body: &Snapshot, edge: EntityRef, distance: f64) -> Result<Snapshot, String> {
    run(
        body,
        KernelCommand::FinishEdges {
            target_edges: vec![edge],
            kind: EdgeFinishKind::Chamfer,
            distance,
            standing_apart: true,
        },
        "bevel standing apart",
    )
}

fn close(measured: f64, expected: f64, what: &str) {
    // Standing apart is a regularized Boolean, not the analytic band, and it
    // does not pretend otherwise: each cut imprints its plane on the faces it
    // meets and sews the pieces, and the last digits come from that arithmetic
    // rather than from a closed form. Measured against the closed form it is
    // right to within a part in a thousand million of the body — three orders
    // inside any modelling tolerance, and worth stating rather than hiding
    // behind a loose absolute.
    let tolerance = expected.abs().max(1.0) * 1.0e-9;
    assert!(
        (measured - expected).abs() <= tolerance,
        "{what}: {measured} against {expected}"
    );
}

#[test]
fn one_bevel_standing_apart_is_the_bevel_it_would_have_been_anyway() {
    // With no corner to argue about, standing apart and joining are the same
    // shape, and this is what says so.
    let distance = 2.0;
    let cube = cube();
    let apart = bevel_apart(&cube, origin_edge(&cube, 2), distance).expect("one bevel");
    close(
        apart.measures().volume,
        SIDE.powi(3) - 0.5 * distance * distance * SIDE,
        "one bevel standing apart",
    );
}

#[test]
fn two_bevels_of_one_corner_taken_apart_claim_its_shared_wedge_once() {
    let distance = 2.0;
    let mut body = cube();
    for axis in [2, 0] {
        let edge = origin_edge(&body, axis);
        body = bevel_apart(&body, edge, distance)
            .unwrap_or_else(|error| panic!("bevel along axis {axis}: {error}"));
    }
    close(
        body.measures().volume,
        SIDE.powi(3) - (distance * distance * SIDE - distance.powi(3) / 3.0),
        "two bevels standing apart",
    );
}

#[test]
fn three_bevels_of_one_corner_taken_apart_leave_a_point_where_they_meet() {
    let distance = 2.0;
    let mut body = cube();
    for axis in [2, 0, 1] {
        let edge = origin_edge(&body, axis);
        body = bevel_apart(&body, edge, distance)
            .unwrap_or_else(|error| panic!("bevel along axis {axis}: {error}"));
    }
    let removed = 1.5 * distance * distance * SIDE - distance.powi(3) + distance.powi(3) / 4.0;
    close(
        body.measures().volume,
        SIDE.powi(3) - removed,
        "three bevels standing apart",
    );
}

/// The whole point of the option: a corner two edges were already finished on,
/// taking the third beside them rather than into them.
///
/// The joined answer to the same selection is the corner patch, which removes
/// more — the three bevel planes meeting at a point of their own is exactly the
/// material a patch would have taken.
#[test]
fn the_third_edge_of_a_finished_corner_stands_apart_from_it() {
    let distance = 2.0;
    let cube = cube();
    let pair = [origin_edge(&cube, 0), origin_edge(&cube, 1)];
    let mitred = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: pair.to_vec(),
            kind: EdgeFinishKind::Chamfer,
            distance,
            standing_apart: false,
        },
        "two edges together",
    )
    .expect("two edges of a corner bevel together");
    close(
        mitred.measures().volume,
        SIDE.powi(3) - (distance * distance * SIDE - distance.powi(3) / 3.0),
        "the mitred pair",
    );

    let third = origin_edge(&mitred, 2);
    let apart = bevel_apart(&mitred, third, distance).expect("the third edge stands apart");
    let removed = 1.5 * distance * distance * SIDE - distance.powi(3) + distance.powi(3) / 4.0;
    close(
        apart.measures().volume,
        SIDE.powi(3) - removed,
        "the third bevel standing apart",
    );

    // And it is a different body from the joined answer, which is the whole
    // reason the panel asks rather than choosing.
    let joined = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![pair[0], pair[1], origin_edge(&cube, 2)],
            kind: EdgeFinishKind::Chamfer,
            distance,
            standing_apart: false,
        },
        "all three together",
    )
    .expect("all three edges bevel together");
    assert!(
        (joined.measures().volume - apart.measures().volume).abs() > 1.0e-6,
        "joined {} and apart {} should not be the same body",
        joined.measures().volume,
        apart.measures().volume
    );
}

#[test]
fn a_fillet_standing_apart_is_refused_by_name_rather_than_approximated() {
    let cube = cube();
    let error = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![origin_edge(&cube, 2)],
            kind: EdgeFinishKind::Fillet,
            distance: 2.0,
            standing_apart: true,
        },
        "fillet standing apart",
    )
    .expect_err("a fillet standing apart is not built yet");
    assert!(
        error.contains("EDGE_FINISH_APART_FILLET_UNSUPPORTED"),
        "the refusal should name itself, and said {error}"
    );
    assert!(
        error.contains("tangent"),
        "and say what actually stops it, not merely that something does: {error}"
    );
}

/// A bevel standing apart still has to be a bevel of something.
#[test]
fn standing_apart_refuses_an_edge_that_is_not_between_two_flat_faces() {
    let distance = 2.0;
    let cube = cube();
    let rounded = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![origin_edge(&cube, 2)],
            kind: EdgeFinishKind::Fillet,
            distance,
            standing_apart: false,
        },
        "round one edge",
    )
    .expect("one edge rounds");

    // The line where the band meets a wall: flat on one side, cylindrical on
    // the other, so there is no second plane to set back into.
    let scene = NativeKernel::debug_scene(&rounded);
    let tangent = scene
        .edges
        .iter()
        .find(|edge| edge.is_tangent)
        .map(|edge| edge.source_edge);
    let Some(tangent) = tangent else {
        return;
    };
    let error = bevel_apart(&rounded, tangent, 0.5)
        .expect_err("a band's own tangency line is not an edge to bevel apart");
    assert!(
        error.contains("EDGE_FINISH_APART"),
        "the refusal should name the route that gave it: {error}"
    );
}

/// The tool has to be built big enough for the body it is cutting, wherever
/// that body happens to sit. A cuboid nowhere near the origin is what says so.
#[test]
fn standing_apart_cuts_a_body_that_is_nowhere_near_the_origin() {
    let distance = 2.0;
    let sides = [SIDE, SIDE * 1.5, SIDE * 0.5];
    let far = run(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(-410.0, 250.0, -90.0),
            size_x: sides[0],
            size_y: sides[1],
            size_z: sides[2],
        },
        "a distant block",
    )
    .expect("a block");
    let whole = sides[0] * sides[1] * sides[2];
    let edge = NativeKernel::debug_scene(&far)
        .edges
        .iter()
        .filter(|edge| !edge.is_smooth)
        .find(|edge| {
            (edge.endpoints[1].z - edge.endpoints[0].z).abs() > 1.0e-9
                && (edge.endpoints[0].x + 410.0).abs() < 1.0e-9
                && (edge.endpoints[0].y - 250.0).abs() < 1.0e-9
        })
        .map(|edge| edge.source_edge)
        .expect("the block has a vertical edge at its near corner");
    let apart = bevel_apart(&far, edge, distance).expect("a distant block bevels too");
    close(
        apart.measures().volume,
        whole - 0.5 * distance * distance * sides[2],
        "one bevel on a distant block",
    );
}

/// A corner that was *rounded* rather than bevelled is refused by name.
///
/// The cut plane would meet those cylinders in ellipses, which the curve
/// vocabulary carries perfectly well — but the Boolean cannot yet sew the
/// result, and a route that cannot make the shape the user asked for says so
/// instead of publishing a different one (ADR 0002).
#[test]
fn a_bevel_standing_apart_from_a_rounded_corner_is_refused_by_name() {
    let distance = 2.0;
    let cube = cube();
    let rounded = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![origin_edge(&cube, 0), origin_edge(&cube, 1)],
            kind: EdgeFinishKind::Fillet,
            distance,
            standing_apart: false,
        },
        "two edges rounded together",
    )
    .expect("two edges of a corner round together");
    let error = bevel_apart(&rounded, origin_edge(&rounded, 2), distance)
        .expect_err("a bevel standing apart from two fillets is not built yet");
    assert!(
        error.contains("EDGE_FINISH_APART"),
        "the refusal should name the route that gave it: {error}"
    );
}

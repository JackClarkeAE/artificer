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

fn round_apart(body: &Snapshot, edge: EntityRef, radius: f64) -> Result<Snapshot, String> {
    run(
        body,
        KernelCommand::FinishEdges {
            target_edges: vec![edge],
            kind: EdgeFinishKind::Fillet,
            distance: radius,
            standing_apart: true,
        },
        "fillet standing apart",
    )
}

/// A bevel standing apart from a corner that was *rounded*: the cut plane
/// meets those bands in ellipses, which the curve vocabulary carries.
#[test]
fn a_bevel_standing_apart_from_a_rounded_corner() {
    let size = 2.0;
    let cube = cube();
    let rounded = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![origin_edge(&cube, 0), origin_edge(&cube, 1)],
            kind: EdgeFinishKind::Fillet,
            distance: size,
            standing_apart: false,
        },
        "two edges rounded together",
    )
    .expect("two edges of a corner round together");
    let before = rounded.measures().volume;
    match bevel_apart(&rounded, origin_edge(&rounded, 2), size) {
        Ok(apart) => assert!(
            apart.measures().volume < before,
            "the bevel removes material: {} against {before}",
            apart.measures().volume
        ),
        Err(error) => assert!(
            error.contains("EDGE_FINISH_APART"),
            "a refusal should name the route that gave it: {error}"
        ),
    }
}

/// What three quarter-round prisms off one corner remove between them.
///
/// Derived here by inclusion and exclusion, not read from the kernel. With
/// `u = r − x` and so on, a band along x covers `v² + w² ≤ r²`, so
///
/// ```text
/// |A|       = r²(1 − π/4)L
/// |A∩B|     = ∫₀ʳ (r − √(r²−w²))² dw  =  r³(5/3 − π/2)
/// |A∩B∩C|   = r³(1 + √2 − 3π/4)
/// ```
///
/// the last being the corner octant outside all three, which follows from the
/// tricylinder Steinmetz volume `8(2 − √2)r³` taken an eighth at a time. The
/// union is `3|A| − 3|A∩B| + |A∩B∩C|`.
fn bands_removed(count: usize, radius: f64, length: f64) -> f64 {
    let one = radius * radius * (1.0 - std::f64::consts::PI / 4.0) * length;
    let pair = radius.powi(3) * (5.0 / 3.0 - std::f64::consts::PI / 2.0);
    let triple =
        radius.powi(3) * (1.0 + std::f64::consts::SQRT_2 - 3.0 * std::f64::consts::PI / 4.0);
    match count {
        1 => one,
        2 => 2.0 * one - pair,
        _ => 3.0 * one - 3.0 * pair + triple,
    }
}

/// A second band running across the first, each standing apart.
///
/// Two bands meeting at a corner leave the same solid whether they are joined
/// or stood apart — the seam of ADR 0043 is exactly where the two removals
/// meet, and nothing is added there. So this has two oracles that must agree:
/// the closed form, and the body the kernel's own joined seam produces.
#[test]
fn a_second_fillet_across_the_first_stands_apart() {
    let radius = 2.0;
    let cube = cube();
    let mut apart = cube.clone();
    for axis in [0, 1] {
        let edge = origin_edge(&apart, axis);
        apart = round_apart(&apart, edge, radius)
            .unwrap_or_else(|error| panic!("fillet along axis {axis}: {error}"));
    }
    close(
        apart.measures().volume,
        SIDE.powi(3) - bands_removed(2, radius, SIDE),
        "two bands standing apart",
    );

    let joined = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![origin_edge(&cube, 0), origin_edge(&cube, 1)],
            kind: EdgeFinishKind::Fillet,
            distance: radius,
            standing_apart: false,
        },
        "two edges rounded together",
    )
    .expect("two edges of a corner round together");
    close(
        apart.measures().volume,
        joined.measures().volume,
        "two bands are one solid whether joined or stood apart",
    );
}

/// The case the option exists for: a corner two edges were rounded on, taking
/// the third beside them rather than into them.
#[test]
fn the_third_edge_of_a_rounded_corner_stands_apart_from_it() {
    let radius = 2.0;
    let cube = cube();
    let rounded = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![origin_edge(&cube, 0), origin_edge(&cube, 1)],
            kind: EdgeFinishKind::Fillet,
            distance: radius,
            standing_apart: false,
        },
        "two edges rounded together",
    )
    .expect("two edges of a corner round together");
    let apart = round_apart(&rounded, origin_edge(&rounded, 2), radius)
        .expect("the third edge rounds beside the seam");
    close(
        apart.measures().volume,
        SIDE.powi(3) - bands_removed(3, radius, SIDE),
        "the third band standing apart",
    );

    // And it is not the joined answer: closing the corner with a sphere octant
    // takes the material these three leave at their meeting point.
    let joined = run(
        &cube,
        KernelCommand::FinishEdges {
            target_edges: vec![
                origin_edge(&cube, 0),
                origin_edge(&cube, 1),
                origin_edge(&cube, 2),
            ],
            kind: EdgeFinishKind::Fillet,
            distance: radius,
            standing_apart: false,
        },
        "all three rounded together",
    )
    .expect("all three edges round together");
    assert!(
        joined.measures().volume < apart.measures().volume,
        "the joined corner removes more: {} against {}",
        joined.measures().volume,
        apart.measures().volume
    );
}

/// The same corner at forty sizes, two bands and three.
///
/// Where two bands touch is found by sampling a curve and refining, and the
/// size of the fillet moves that touch around — relative to the sample grid,
/// and relative to the vertices it lands near. A rule that turned on a sample
/// falling close enough, or on two ends agreeing to the last bit, would cut
/// some of these and refuse others with nothing geometric to separate them.
/// That is not a domain limit but arithmetic showing through, and it is how
/// one machine came to disagree with another about whether two shapes touch.
#[test]
fn a_corner_stood_apart_cuts_at_every_size() {
    let mut trouble = Vec::new();
    for step in 1..=40 {
        let radius = f64::from(step) * 0.1;
        for bands in [2_usize, 3] {
            let mut body = cube();
            let mut refused = None;
            for axis in 0..bands {
                let edge = origin_edge(&body, axis);
                match round_apart(&body, edge, radius) {
                    Ok(next) => body = next,
                    Err(error) => {
                        refused = Some(format!("{radius:.1}, {bands} bands, axis {axis}: {error}"));
                        break;
                    }
                }
            }
            match refused {
                Some(reason) => trouble.push(reason),
                None => {
                    let want = SIDE.powi(3) - bands_removed(bands, radius, SIDE);
                    if (body.measures().volume - want).abs() > 1.0e-6 {
                        trouble.push(format!(
                            "{radius:.1}, {bands} bands: volume {} wanted {want}",
                            body.measures().volume
                        ));
                    }
                }
            }
        }
    }
    assert!(
        trouble.is_empty(),
        "{} of 80 corners refused or drifted:\n{}",
        trouble.len(),
        trouble.join("\n")
    );
}

/// Three edges of one corner, each stood apart from the others, in every
/// order. The answer cannot depend on which was taken first.
#[test]
fn three_bands_standing_apart_are_the_same_corner_whatever_the_order() {
    let radius = 2.0;
    let expected = SIDE.powi(3) - bands_removed(3, radius, SIDE);
    for order in [[0, 1, 2], [2, 0, 1], [1, 2, 0], [2, 1, 0]] {
        let mut body = cube();
        for axis in order {
            let edge = origin_edge(&body, axis);
            body = round_apart(&body, edge, radius)
                .unwrap_or_else(|error| panic!("{order:?}: fillet along axis {axis}: {error}"));
        }
        close(
            body.measures().volume,
            expected,
            &format!("three bands standing apart, taken {order:?}"),
        );
    }
}

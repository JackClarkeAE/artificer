//! The motion timeline gate: a mechanism measured at every frame, with the
//! frame two parts first meet marked.

use std::collections::BTreeMap;

use artificer_kernel::api::analysis::Subject;
use artificer_kernel::api::interference::{ClearanceState, Placement};
use artificer_kernel::api::session::Session;
use artificer_kernel::api::sweep::SweepStep;
use artificer_kernel::{CancellationToken, Snapshot};
use artificer_protocol::PrecisionPolicy;
use artificer_sim::clearance_timeline;

fn cuboid(size: [f64; 3]) -> Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(
        &format!(
            "let b = box(origin: [0, 0, 0], size: [{}, {}, {}], label: \"b\");\n",
            size[0], size[1], size[2]
        ),
        &BTreeMap::new(),
        &CancellationToken::default(),
    );
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

/// A fixed block and a second block sliding towards it along x: 20 mm
/// apart at frame 0, one millimetre closer each frame, touching at frame
/// 20 and overlapping from frame 21.
fn sliding_blocks() -> (Vec<Subject>, Vec<SweepStep>) {
    let block = cuboid([10.0, 10.0, 10.0]);
    let subjects = vec![
        Subject::new("frame", block.clone()),
        Subject::new("slider", block),
    ];
    let steps = (0..30)
        .map(|frame| {
            let x = 30.0 - frame as f64;
            SweepStep::new(
                vec![frame as f64],
                vec![
                    Placement::IDENTITY,
                    Placement::from_quaternion([1.0, 0.0, 0.0, 0.0], [x, 0.0, 0.0]).unwrap(),
                ],
            )
        })
        .collect();
    (subjects, steps)
}

#[test]
fn every_frame_is_measured_and_the_first_collision_is_named() {
    let (subjects, steps) = sliding_blocks();
    let mut reported = Vec::new();
    let timeline = clearance_timeline(
        &subjects,
        &steps,
        PrecisionPolicy::default(),
        &CancellationToken::default(),
        &mut |step, total| reported.push((step, total)),
    );
    assert_eq!(timeline.steps_offered, 30);
    assert_eq!(timeline.steps_measured, 30);
    assert_eq!(timeline.frames.len(), 30);
    assert!(!timeline.cancelled);
    assert_eq!(reported.len(), 30);
    assert_eq!(reported[0], (0, 30));
    for frame in &timeline.frames[..20] {
        let expected = 20.0 - frame.step as f64;
        assert!(
            (frame.distance - expected).abs() < 1.0e-9,
            "frame {}: {} against {expected}",
            frame.step,
            frame.distance
        );
        assert_eq!(frame.state, ClearanceState::Clear);
        assert!(!frame.may_overlap);
        assert_eq!(frame.pair, (0, 1));
        assert_eq!(frame.drivers, vec![frame.step as f64]);
    }
    assert_eq!(timeline.frames[20].state, ClearanceState::Touching);
    assert!(timeline.frames[20].distance <= 1.0e-9);
    assert_eq!(timeline.frames[21].state, ClearanceState::Interfering);
    assert_eq!(timeline.first_collision, Some(21));
    // The tightest clear frame is the one just before contact.
    assert_eq!(timeline.tightest_clear().map(|frame| frame.step), Some(19));
    assert!((timeline.largest() - 20.0).abs() < 1.0e-9);
}

#[test]
fn a_cancelled_timeline_keeps_the_frames_it_measured() {
    let (subjects, steps) = sliding_blocks();
    let token = CancellationToken::default();
    let mut measured = 0;
    let timeline = clearance_timeline(
        &subjects,
        &steps,
        PrecisionPolicy::default(),
        &token,
        &mut |step, _| {
            measured = step;
            if step == 4 {
                token.cancel();
            }
        },
    );
    assert!(timeline.cancelled);
    assert_eq!(timeline.frames.len(), 5, "frames 0 to 4 were measured");
    assert_eq!(timeline.steps_measured, 5);
    assert_eq!(timeline.first_collision, None);
}

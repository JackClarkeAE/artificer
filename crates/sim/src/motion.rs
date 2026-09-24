//! The clearance of a mechanism at every frame of its motion.
//!
//! The kernel's sweep answers "does it fit anywhere it can go" and stops
//! at the first collision. A timeline answers a different question: how
//! close does it come at each moment, plotted against time, with the
//! frame two parts first meet marked on it. So every step is measured,
//! including the ones past a collision — those are not poses the real
//! mechanism reaches, and the timeline says so, but a plot with a hole in
//! it is not a plot.

use std::collections::BTreeMap;

use artificer_kernel::api::analysis::Subject;
use artificer_kernel::api::interference::{ClearanceState, FacetIndex, Placement, clearance};
use artificer_kernel::api::sweep::SweepStep;
use artificer_kernel::{CancellationToken, ChordDeviation, DebugScene, NativeKernel};
use artificer_protocol::{EntityRef, PrecisionPolicy, Tier};

/// The tightest reading at one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameClearance {
    pub step: usize,
    /// The joint coordinates at this frame, as the caller gave them.
    pub drivers: Vec<f64>,
    /// The closest any two subjects come, in millimetres; zero when they
    /// touch or overlap.
    pub distance: f64,
    /// The worst state any pair is in at this frame.
    pub state: ClearanceState,
    /// The pair that reading belongs to, as subject indices.
    pub pair: (usize, usize),
    /// Whether the facets cannot rule out overlap at this frame.
    pub may_overlap: bool,
    pub tier: Tier,
}

/// Every frame of a motion, measured.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionTimeline {
    pub frames: Vec<FrameClearance>,
    /// The first frame two parts may share space, if any.
    pub first_collision: Option<usize>,
    pub steps_offered: usize,
    pub steps_measured: usize,
    pub cancelled: bool,
    pub tier: Tier,
}

impl MotionTimeline {
    /// The tightest frame that is still clear.
    #[must_use]
    pub fn tightest_clear(&self) -> Option<&FrameClearance> {
        self.frames
            .iter()
            .filter(|frame| frame.state == ClearanceState::Clear && frame.distance.is_finite())
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
    }

    /// The largest finite clearance on the timeline, for a plot's scale.
    #[must_use]
    pub fn largest(&self) -> f64 {
        self.frames
            .iter()
            .map(|frame| frame.distance)
            .filter(|distance| distance.is_finite())
            .fold(0.0, f64::max)
    }
}

/// Measures the clearance of every pair at every step.
///
/// `progress` is called with the step about to be measured and the total.
#[must_use]
pub fn clearance_timeline(
    subjects: &[Subject],
    steps: &[SweepStep],
    precision: PrecisionPolicy,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(usize, usize),
) -> MotionTimeline {
    let scenes = subjects
        .iter()
        .map(|subject| NativeKernel::debug_scene(&subject.snapshot))
        .collect::<Vec<_>>();
    let deviations = subjects
        .iter()
        .map(|subject| NativeKernel::display_chord_deviations(&subject.snapshot))
        .collect::<Vec<_>>();
    let mut cache = IndexCache::default();
    let mut frames = Vec::with_capacity(steps.len());
    let mut first_collision = None;
    let mut cancelled = false;
    let mut tier = Tier::Exact;
    let mut measured = 0;
    for (index, step) in steps.iter().enumerate() {
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
        progress(index, steps.len());
        if step.placements.len() != subjects.len() || subjects.len() < 2 {
            continue;
        }
        let indices = (0..subjects.len())
            .map(|subject| {
                cache.index(
                    subject,
                    step.placements[subject],
                    &scenes[subject],
                    &deviations[subject],
                )
            })
            .collect::<Vec<_>>();
        measured += 1;
        let mut frame = FrameClearance {
            step: index,
            drivers: step.drivers.clone(),
            distance: f64::INFINITY,
            state: ClearanceState::Clear,
            pair: (0, 1),
            may_overlap: false,
            tier: Tier::Exact,
        };
        for first in 0..subjects.len() {
            for second in first + 1..subjects.len() {
                let report = clearance(&indices[first], &indices[second], precision);
                tier = tier.combine(report.tier);
                frame.tier = frame.tier.combine(report.tier);
                let worse = rank(report.state) < rank(frame.state)
                    || (report.state == frame.state && report.distance < frame.distance);
                if worse {
                    frame.distance = report.distance;
                    frame.state = report.state;
                    frame.pair = (first, second);
                }
                if report.may_overlap() {
                    frame.may_overlap = true;
                }
            }
        }
        if frame.may_overlap && first_collision.is_none() {
            first_collision = Some(index);
        }
        frames.push(frame);
    }
    MotionTimeline {
        frames,
        first_collision,
        steps_offered: steps.len(),
        steps_measured: measured,
        cancelled,
        tier,
    }
}

const fn rank(state: ClearanceState) -> u8 {
    match state {
        ClearanceState::Interfering => 0,
        ClearanceState::Touching => 1,
        ClearanceState::Clear => 2,
    }
}

/// One facet hierarchy per subject, rebuilt only when that subject moves.
#[derive(Default)]
struct IndexCache {
    held: Vec<Option<(Placement, FacetIndex)>>,
}

impl IndexCache {
    fn index(
        &mut self,
        subject: usize,
        placement: Placement,
        scene: &DebugScene,
        deviations: &BTreeMap<EntityRef, ChordDeviation>,
    ) -> FacetIndex {
        if self.held.len() <= subject {
            self.held.resize_with(subject + 1, || None);
        }
        if let Some((held, index)) = self.held[subject].as_ref()
            && *held == placement
        {
            return index.clone();
        }
        let index = FacetIndex::from_scene(scene, placement, deviations);
        self.held[subject] = Some((placement, index.clone()));
        index
    }
}

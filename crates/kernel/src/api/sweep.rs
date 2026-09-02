//! Interference over a motion, not over a pose.
//!
//! A static study answers "do these parts fit where they are". A mechanism
//! asks a harder question: does it fit *anywhere it can go*. A sweep is the
//! answer — the assembly stepped through its travel, measured at every
//! step, stopping the moment two parts share space.
//!
//! ## What a step is
//!
//! A step is where every body sits at one position of the mechanism. This
//! module does not know what a joint is: the caller solves the mechanism
//! and hands over the placements, which is what keeps the kernel free of
//! the document's assembly vocabulary and lets a sweep run over poses that
//! came from anywhere — a solver, a recording, a file.
//!
//! ## What comes back
//!
//! Two things. A report, which is the document: every pair at the step it
//! came closest, the first collision if there was one, and the verdict a
//! clearance profile gives. And a field per subject, which is the picture:
//! the worst reading each facet corner saw anywhere in the motion. A
//! mechanism that never collides is painted by how close it came; one that
//! does is painted by where it collided, because the sweep stops there.

use std::time::Instant;

use artificer_protocol::{Point3, PrecisionPolicy, Tier};
use serde::{Deserialize, Serialize};

use crate::api::analysis::{ClearanceProfile, FitVerdict, Subject};
use crate::api::interference::{ClearanceState, FacetIndex, Placement, clearance, clearance_field};
use crate::{CancellationToken, DebugScene, NativeKernel};

/// The shape of the document this module publishes.
pub const SWEEP_SCHEMA_VERSION: u32 = 1;

/// One position of the mechanism: where every subject sits, in subject
/// order, and the driver values that put it there.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepStep {
    /// The joint coordinates this step stands for, for the report to name
    /// the position a collision was found at. The kernel does not
    /// interpret them.
    pub drivers: Vec<f64>,
    pub placements: Vec<Placement>,
}

impl SweepStep {
    #[must_use]
    pub fn new(drivers: Vec<f64>, placements: Vec<Placement>) -> Self {
        Self {
            drivers,
            placements,
        }
    }
}

/// One pair of bodies over the whole motion, at the step it came closest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SweptPair {
    pub a: String,
    pub b: String,
    /// The closest the two ever came, in millimetres.
    pub distance: f64,
    /// The step that reading was taken at, and the drivers there.
    pub step: usize,
    pub drivers: Vec<f64>,
    pub state: ClearanceState,
    pub witness_a: Point3,
    pub witness_b: Point3,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bound: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<FitVerdict>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// Where a sweep first found two bodies sharing space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SweepCollision {
    pub step: usize,
    pub drivers: Vec<f64>,
    pub a: String,
    pub b: String,
    pub witness_a: Point3,
    pub witness_b: Point3,
}

/// The published answer to whether a mechanism clears itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SweepReport {
    pub schema_version: u32,
    pub kernel_version: String,
    pub subjects: Vec<String>,
    /// How many steps the caller offered, and how many were measured. They
    /// differ when a collision stopped the sweep or it was cancelled, and
    /// the difference is the part of the travel nothing is known about.
    pub steps_offered: usize,
    pub steps_measured: usize,
    /// The first collision, which is also where the sweep stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision: Option<SweepCollision>,
    /// Every pair, at the step it came closest, worst first.
    pub pairs: Vec<SweptPair>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ClearanceProfile>,
    /// Pairs the profile called too close at their tightest step.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub failing: usize,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

fn is_zero_count(value: &usize) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl SweepReport {
    /// The tightest the mechanism ever came to itself without touching.
    #[must_use]
    pub fn tightest(&self) -> Option<&SweptPair> {
        self.pairs
            .iter()
            .filter(|pair| pair.state == ClearanceState::Clear && pair.distance.is_finite())
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
    }

    /// Whether the mechanism clears itself through every step measured.
    #[must_use]
    pub fn clears(&self) -> bool {
        self.collision.is_none() && self.failing == 0 && !self.cancelled
    }
}

/// A sweep's report and the picture that goes with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    pub report: SweepReport,
    /// The worst reading each facet corner saw anywhere in the motion, one
    /// field per subject in subject order, in scene order within each.
    ///
    /// Worst rather than last, so a heat map of a whole travel shows the
    /// tightest the mechanism ever got at each point on each part rather
    /// than wherever it happened to stop.
    pub fields: Vec<Vec<f64>>,
}

/// Sweeps an assembly through a motion, measuring at every step.
///
/// The sweep stops at the first collision. It is looking for whether the
/// mechanism can move, and once the answer is no, the rest of the travel
/// is a different mechanism's question: the parts have already passed
/// through one another, so nothing after that point is a pose the real
/// thing ever reaches.
///
/// `progress` is called with the step about to be measured and the total,
/// so a caller running this off the UI thread has something to show.
#[must_use]
pub fn interference_sweep(
    subjects: &[Subject],
    steps: &[SweepStep],
    precision: PrecisionPolicy,
    profile: Option<&ClearanceProfile>,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(usize, usize),
) -> Sweep {
    let started = Instant::now();
    let scenes = subjects
        .iter()
        .map(|subject| NativeKernel::debug_scene(&subject.snapshot))
        .collect::<Vec<_>>();
    let polyhedral = subjects
        .iter()
        .map(|subject| NativeKernel::is_polyhedral(&subject.snapshot))
        .collect::<Vec<_>>();
    let budget = subjects
        .iter()
        .map(|subject| {
            let policy = subject.snapshot.precision_policy().unwrap_or_default();
            policy.approximation_budget.max(policy.modeling_resolution)
        })
        .collect::<Vec<_>>();

    let mut fields = scenes
        .iter()
        .map(|scene| vec![f64::INFINITY; scene.triangles.len() * 3])
        .collect::<Vec<_>>();
    let mut best: Vec<Option<SweptPair>> = Vec::new();
    let mut collision = None;
    let mut tier = Tier::Exact;
    let mut measured = 0;
    let mut cancelled = false;
    let mut cache = IndexCache::default();

    'sweep: for (index, step) in steps.iter().enumerate() {
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
        progress(index, steps.len());
        // A step that does not place every subject is not a position of
        // this assembly, so it is skipped rather than half-applied.
        if step.placements.len() != subjects.len() {
            continue;
        }
        let indices = (0..subjects.len())
            .map(|subject| {
                cache.index(
                    subject,
                    step.placements[subject],
                    &scenes[subject],
                    polyhedral[subject],
                    budget[subject],
                )
            })
            .collect::<Vec<_>>();
        measured += 1;

        for (first, subject) in subjects.iter().enumerate() {
            for (second, other) in subjects.iter().enumerate().skip(first + 1) {
                let report = clearance(&indices[first], &indices[second], precision);
                tier = tier.combine(report.tier);
                let slot = pair_slot(&mut best, first, second, subjects.len());
                let closer = best[slot]
                    .as_ref()
                    .is_none_or(|held| report.distance < held.distance);
                if closer {
                    best[slot] = Some(SweptPair {
                        a: subject.name.clone(),
                        b: other.name.clone(),
                        distance: report.distance,
                        step: index,
                        drivers: step.drivers.clone(),
                        state: report.state,
                        witness_a: report.witness_a,
                        witness_b: report.witness_b,
                        tier: report.tier,
                        bound: report.bound,
                        verdict: profile
                            .map(|profile| FitVerdict::of(report.state, report.distance, profile)),
                    });
                }
                if report.state == ClearanceState::Interfering && collision.is_none() {
                    collision = Some(SweepCollision {
                        step: index,
                        drivers: step.drivers.clone(),
                        a: subject.name.clone(),
                        b: other.name.clone(),
                        witness_a: report.witness_a,
                        witness_b: report.witness_b,
                    });
                }
            }
        }

        // The picture, accumulated before the stop so the colliding step is
        // the one painted.
        for (subject, field) in fields.iter_mut().enumerate() {
            let others = (0..subjects.len())
                .filter(|other| *other != subject)
                .map(|other| &indices[other])
                .collect::<Vec<_>>();
            let step_field = clearance_field(&scenes[subject], step.placements[subject], &others);
            for (held, reading) in field.iter_mut().zip(step_field) {
                if reading < *held {
                    *held = reading;
                }
            }
        }

        if collision.is_some() {
            break 'sweep;
        }
    }

    let mut pairs = best.into_iter().flatten().collect::<Vec<_>>();
    pairs.sort_by(|left, right| {
        let rank = |state: ClearanceState| match state {
            ClearanceState::Interfering => 0,
            ClearanceState::Touching => 1,
            ClearanceState::Clear => 2,
        };
        rank(left.state)
            .cmp(&rank(right.state))
            .then(left.distance.total_cmp(&right.distance))
    });
    let failing = pairs
        .iter()
        .filter(|pair| pair.verdict == Some(FitVerdict::TooClose))
        .count();

    Sweep {
        report: SweepReport {
            schema_version: SWEEP_SCHEMA_VERSION,
            kernel_version: env!("CARGO_PKG_VERSION").to_owned(),
            subjects: subjects
                .iter()
                .map(|subject| subject.name.clone())
                .collect(),
            steps_offered: steps.len(),
            steps_measured: measured,
            collision,
            pairs,
            profile: profile.cloned(),
            failing,
            tier,
            cancelled,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        fields,
    }
}

/// The slot one unordered pair keeps across the whole sweep.
fn pair_slot(
    best: &mut Vec<Option<SweptPair>>,
    first: usize,
    second: usize,
    subjects: usize,
) -> usize {
    if best.is_empty() {
        best.resize(subjects * subjects, None);
    }
    first * subjects + second
}

/// One facet hierarchy per subject, rebuilt only when that subject moves.
///
/// The hierarchy is built over placed facets, so a body that moves needs a
/// new one every step. A body that does not — the frame a mechanism turns
/// against, which is most of an assembly — keeps the one it had, and that
/// is the difference between a sweep costing one build per subject and one
/// per subject per step.
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
        exact: bool,
        budget: f64,
    ) -> FacetIndex {
        if self.held.len() <= subject {
            self.held.resize_with(subject + 1, || None);
        }
        if let Some((held, index)) = self.held[subject].as_ref()
            && *held == placement
        {
            return index.clone();
        }
        let index = FacetIndex::from_scene(scene, placement, exact, budget);
        self.held[subject] = Some((placement, index.clone()));
        index
    }
}

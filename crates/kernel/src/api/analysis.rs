//! Interference studies: every pair of bodies, measured and judged.
//!
//! A study is the machine-readable answer to "does this assembly fit". It
//! names its subjects, measures each pair through
//! [`crate::api::interference`], and publishes a versioned document with
//! the same promises the session report makes: every number says which
//! tier it belongs to, an approximate one carries the bound it can be
//! wrong by, and anything the kernel could not answer says so by name
//! rather than being left out.
//!
//! No Boolean is needed to run one. The overlap *volume* of an interfering
//! pair is the one figure that is, and it is attempted only for the pairs
//! that interfere; where the Boolean engine refuses, the pair keeps its
//! measured clearance and records why the volume is missing.

use std::time::Instant;

use artificer_protocol::{
    BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, Point3, PrecisionPolicy, RequestId,
    Tier,
};
use serde::{Deserialize, Serialize};

use crate::api::interference::{ClearanceState, FacetIndex, Placement, clearance, clearance_field};
use crate::{CancellationToken, NativeKernel, Snapshot};

/// The shape of the document this module publishes. A reader that
/// understands version `n` can refuse anything else outright.
///
/// Version 2 added the clearance profile a study was judged against and
/// the per-pair verdict that follows from it.
pub const ANALYSIS_SCHEMA_VERSION: u32 = 2;

/// A fit the assembly is being checked against: how small a gap is too
/// small, and how large a gap is larger than the fit needed.
///
/// The window is what turns a measurement into an answer. "0.42 mm" says
/// nothing on its own; "0.42 mm, and this press fit wants 0.10 to 0.20"
/// says the part is loose, and "0.02 mm" against the same window says it
/// will not go together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClearanceProfile {
    /// A stable key, so a document can store a choice and a report can
    /// name it without depending on the display name.
    pub key: String,
    pub name: String,
    /// The smallest gap that passes, in millimetres.
    pub minimum: f64,
    /// The largest gap the fit needs, in millimetres. Absent where the fit
    /// has no upper complaint, which is what a plain "do these parts
    /// clash" check is. An absent bound rather than an infinite one
    /// because infinity is not a JSON number and this document is
    /// published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// What the profile is for, in one line.
    pub note: String,
}

/// A profile the kernel ships, as a table entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuiltInProfile {
    pub key: &'static str,
    pub name: &'static str,
    pub minimum: f64,
    pub maximum: Option<f64>,
    pub note: &'static str,
}

impl BuiltInProfile {
    #[must_use]
    pub fn profile(self) -> ClearanceProfile {
        ClearanceProfile {
            key: self.key.to_owned(),
            name: self.name.to_owned(),
            minimum: self.minimum,
            maximum: self.maximum,
            note: self.note.to_owned(),
        }
    }
}

/// The profiles the kernel ships, loosest tolerance last.
///
/// These are process figures, not opinions about a design: the printed
/// ones are the gaps those processes need before two parts that were
/// modelled to touch will actually go together, and the machined one is
/// an ordinary running fit. A design with its own numbers builds its own
/// [`ClearanceProfile`]; nothing here is privileged.
pub const BUILT_IN_PROFILES: [BuiltInProfile; 5] = [
    BuiltInProfile {
        key: "machined-running",
        name: "Machined running fit",
        minimum: 0.02,
        maximum: Some(0.08),
        note: "A milled or turned part that has to turn or slide in service.",
    },
    BuiltInProfile {
        key: "resin-fine",
        name: "Resin fine fit",
        minimum: 0.05,
        maximum: Some(0.15),
        note: "Masked stereolithography, where the layer is thin and the part is stiff.",
    },
    BuiltInProfile {
        key: "fdm-press",
        name: "FDM press fit",
        minimum: 0.10,
        maximum: Some(0.20),
        note: "A fused-filament part meant to be pushed together and stay together.",
    },
    BuiltInProfile {
        key: "fdm-sliding",
        name: "FDM sliding fit",
        minimum: 0.30,
        maximum: Some(0.50),
        note: "A fused-filament part that has to move after it is assembled.",
    },
    BuiltInProfile {
        key: "assembly",
        name: "Assembly check",
        minimum: 0.0,
        maximum: None,
        note: "No fit at all: parts must simply not occupy the same space.",
    },
];

/// Looks a shipped profile up by its key.
#[must_use]
pub fn built_in_profile(key: &str) -> Option<ClearanceProfile> {
    BUILT_IN_PROFILES
        .iter()
        .find(|profile| profile.key == key)
        .map(|profile| profile.profile())
}

/// What one pair's closest approach means under a profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitVerdict {
    /// The gap sits inside the window: the fit is the one that was asked
    /// for.
    Pass,
    /// Closer than the profile allows, or the bodies overlap outright.
    /// This is the only verdict that fails a study.
    TooClose,
    /// Clear by more than the fit needed. Not a failure — a part that was
    /// meant to be held and is not.
    Loose,
}

impl FitVerdict {
    /// The verdict a measured pair earns under a profile.
    ///
    /// Contact is judged on the measurement, not on the state: two bodies
    /// that touch have a gap of zero, and zero is below every window whose
    /// minimum is positive. A profile that asks for no gap at all passes
    /// them, which is what an assembly check means.
    #[must_use]
    pub fn of(state: ClearanceState, distance: f64, profile: &ClearanceProfile) -> Self {
        if state == ClearanceState::Interfering {
            return Self::TooClose;
        }
        if distance < profile.minimum {
            Self::TooClose
        } else if profile.maximum.is_none_or(|maximum| distance <= maximum) {
            Self::Pass
        } else {
            Self::Loose
        }
    }
}

/// One body taking part in a study, at the placement it occupies.
#[derive(Clone, Debug)]
pub struct Subject {
    pub name: String,
    pub snapshot: Snapshot,
    pub placement: Placement,
}

impl Subject {
    #[must_use]
    pub fn new(name: impl Into<String>, snapshot: Snapshot) -> Self {
        Self {
            name: name.into(),
            snapshot,
            placement: Placement::IDENTITY,
        }
    }

    #[must_use]
    pub const fn at(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }
}

/// What one pair of bodies came back with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairReport {
    pub a: String,
    pub b: String,
    pub state: ClearanceState,
    /// The closest approach of the two surfaces, in millimetres.
    pub distance: f64,
    pub witness_a: Point3,
    pub witness_b: Point3,
    pub tier: Tier,
    /// How far below `distance` the true clearance may sit, from the chord
    /// budget of each curved body. Zero when the pair is exact.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bound: f64,
    /// The volume the two bodies share, for a pair that interferes and
    /// whose operands the Boolean engine can carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_volume: Option<f64>,
    /// Why the overlap volume is absent, when it is and the pair
    /// interferes: the Boolean engine's own refusal code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_unavailable: Option<String>,
    /// What this pair's closest approach means under the study's profile.
    /// Absent when the study was run without one, because a measurement
    /// with nothing to be measured against is not a pass or a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<FitVerdict>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// A whole study, ready to read or to serialise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterferenceReport {
    pub schema_version: u32,
    pub kernel_version: String,
    /// Every subject, in the order they were given.
    pub subjects: Vec<String>,
    /// Every unordered pair, in subject order.
    pub pairs: Vec<PairReport>,
    /// How many pairs interfere, touch, and are clear.
    pub interfering: usize,
    pub touching: usize,
    pub clear: usize,
    /// The tightest clearance among the pairs that are clear, and the pair
    /// it belongs to. Absent when no pair is clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tightest: Option<Tightest>,
    /// The fit this study was judged against, when it was judged at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ClearanceProfile>,
    /// How many pairs the profile calls too close. Zero is the study
    /// passing; anything else is the count of the fits that do not work.
    /// Zero also when there is no profile, which is why `profile` is what
    /// a reader checks before believing it.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub failing: usize,
    /// How many pairs are clear by more than the fit needed.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub loose: usize,
    /// `approximate` when any pair's measurement was, so a reader can tell
    /// at a glance whether the study rests on chords anywhere.
    pub tier: Tier,
    pub elapsed_ms: u64,
}

fn is_zero_count(value: &usize) -> bool {
    *value == 0
}

impl InterferenceReport {
    /// Judges every pair against a profile, in place.
    ///
    /// Nothing is measured again: the closest approach of each pair is
    /// already the number a fit is decided on, so changing the fit changes
    /// the verdicts and nothing else. Passing `None` withdraws the
    /// judgement and leaves the measurements standing.
    pub fn judge(&mut self, profile: Option<ClearanceProfile>) {
        for pair in &mut self.pairs {
            pair.verdict = profile
                .as_ref()
                .map(|profile| FitVerdict::of(pair.state, pair.distance, profile));
        }
        self.failing = self.verdicts(FitVerdict::TooClose);
        self.loose = self.verdicts(FitVerdict::Loose);
        self.profile = profile;
    }

    fn verdicts(&self, verdict: FitVerdict) -> usize {
        self.pairs
            .iter()
            .filter(|pair| pair.verdict == Some(verdict))
            .count()
    }

    /// The pair a profile fails on hardest: the tightest of the pairs it
    /// calls too close. Its witness points are where on the two bodies
    /// that worst reading was taken.
    #[must_use]
    pub fn worst_fit(&self) -> Option<&PairReport> {
        self.pairs
            .iter()
            .filter(|pair| pair.verdict == Some(FitVerdict::TooClose))
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
    }
}

/// The closest that any pair which is not in contact comes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tightest {
    pub a: String,
    pub b: String,
    pub distance: f64,
}

/// Measures every pair of subjects.
///
/// The index for each subject is built once and reused across its pairs,
/// which is what keeps a study over many bodies proportional to the pairs
/// rather than to the pairs times the tessellation.
#[must_use]
pub fn interference_study(
    subjects: &[Subject],
    precision: PrecisionPolicy,
    cancellation: &CancellationToken,
) -> InterferenceReport {
    let started = Instant::now();
    let indices = subjects
        .iter()
        .map(|subject| FacetIndex::build(&subject.snapshot, subject.placement))
        .collect::<Vec<_>>();

    let mut pairs = Vec::new();
    let mut tier = Tier::Exact;
    for (first, subject) in subjects.iter().enumerate() {
        for (second, other) in subjects.iter().enumerate().skip(first + 1) {
            if cancellation.is_cancelled() {
                break;
            }
            let report = clearance(&indices[first], &indices[second], precision);
            tier = tier.combine(report.tier);
            let (overlap_volume, overlap_unavailable) =
                if report.state == ClearanceState::Interfering {
                    overlap(subject, other, precision, cancellation)
                } else {
                    (None, None)
                };
            pairs.push(PairReport {
                a: subject.name.clone(),
                b: other.name.clone(),
                state: report.state,
                distance: report.distance,
                witness_a: report.witness_a,
                witness_b: report.witness_b,
                tier: report.tier,
                bound: report.bound,
                overlap_volume,
                overlap_unavailable,
                verdict: None,
            });
        }
    }

    let interfering = count(&pairs, ClearanceState::Interfering);
    let touching = count(&pairs, ClearanceState::Touching);
    let clear = count(&pairs, ClearanceState::Clear);
    let tightest = pairs
        .iter()
        .filter(|pair| pair.state == ClearanceState::Clear && pair.distance.is_finite())
        .min_by(|left, right| left.distance.total_cmp(&right.distance))
        .map(|pair| Tightest {
            a: pair.a.clone(),
            b: pair.b.clone(),
            distance: pair.distance,
        });

    InterferenceReport {
        schema_version: ANALYSIS_SCHEMA_VERSION,
        kernel_version: env!("CARGO_PKG_VERSION").to_owned(),
        subjects: subjects
            .iter()
            .map(|subject| subject.name.clone())
            .collect(),
        pairs,
        interfering,
        touching,
        clear,
        tightest,
        profile: None,
        failing: 0,
        loose: 0,
        tier,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

/// The heat map of a whole study: for each subject, the signed clearance
/// from every corner of its display facets to the nearest of the others, in
/// scene order.
///
/// One index per subject is built and shared by every subject that reads it,
/// so `n` bodies cost `n` builds rather than `n(n - 1)`. A cancelled study
/// leaves the fields it had not reached empty rather than partly filled: a
/// half-measured body would paint as clear where nothing had looked yet.
#[must_use]
pub fn clearance_fields(subjects: &[Subject], cancellation: &CancellationToken) -> Vec<Vec<f64>> {
    let indices = subjects
        .iter()
        .map(|subject| FacetIndex::build(&subject.snapshot, subject.placement))
        .collect::<Vec<_>>();
    subjects
        .iter()
        .enumerate()
        .map(|(current, subject)| {
            if cancellation.is_cancelled() {
                return Vec::new();
            }
            let others = indices
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != current)
                .map(|(_, index)| index)
                .collect::<Vec<_>>();
            let scene = NativeKernel::debug_scene(&subject.snapshot);
            clearance_field(&scene, subject.placement, &others)
        })
        .collect()
}

fn count(pairs: &[PairReport], state: ClearanceState) -> usize {
    pairs.iter().filter(|pair| pair.state == state).count()
}

/// The volume two interfering bodies share, when the Boolean engine can
/// carry them. A refusal is recorded by its code rather than discarded:
/// "the parts overlap and this is how much" and "the parts overlap and the
/// engine could not say how much" are different answers.
fn overlap(
    a: &Subject,
    b: &Subject,
    precision: PrecisionPolicy,
    cancellation: &CancellationToken,
) -> (Option<f64>, Option<String>) {
    let Some(first) = placed(&a.snapshot, a.placement, precision, cancellation) else {
        return (None, Some("PLACEMENT_UNSUPPORTED".to_owned()));
    };
    let Some(second) = placed(&b.snapshot, b.placement, precision, cancellation) else {
        return (None, Some("PLACEMENT_UNSUPPORTED".to_owned()));
    };
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("analysis::overlap"),
        expected_target_snapshot: first.id(),
        expected_tool_snapshot: second.id(),
        precision,
        operation: BooleanOperation::Intersection,
    };
    match NativeKernel::execute_boolean(&first, &second, &request, cancellation) {
        Ok(outcome) => (Some(outcome.snapshot.measures().volume), None),
        Err(error) => (
            None,
            Some(
                error
                    .diagnostics
                    .first()
                    .map_or_else(|| error.code.to_string(), |first| first.code.to_string()),
            ),
        ),
    }
}

/// The snapshot a subject occupies in the world, moving it first when its
/// placement is not the identity.
fn placed(
    snapshot: &Snapshot,
    placement: Placement,
    precision: PrecisionPolicy,
    cancellation: &CancellationToken,
) -> Option<Snapshot> {
    if placement == Placement::IDENTITY {
        return Some(snapshot.clone());
    }
    let transform = placement.to_similarity()?;
    let request = artificer_protocol::ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("analysis::place"),
        expected_snapshot: snapshot.id(),
        precision,
        command: artificer_protocol::KernelCommand::TransformSnapshot { transform },
    };
    NativeKernel::execute(snapshot, &request, cancellation)
        .ok()
        .map(|outcome| outcome.snapshot)
}

/// A study over named steps of a session.
///
/// Each label names the body that step left behind, which is what a
/// script means by a part: `union(target: plate, tool: boss)` leaves one,
/// and the two operands before it leave their own.
pub fn study_session_steps(
    session: &crate::api::session::Session,
    steps: &[String],
    cancellation: &CancellationToken,
) -> Result<InterferenceReport, crate::api::debug::ApiError> {
    use crate::api::debug::{ApiError, ApiErrorCode};

    if steps.len() < 2 {
        return Err(ApiError::new(
            ApiErrorCode::InvalidInput,
            "An interference study needs at least two bodies to compare",
        ));
    }
    let mut subjects = Vec::with_capacity(steps.len());
    for label in steps {
        let id = session.step_snapshots.get(label).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SelectorNotFound,
                format!("Step \"{label}\" is not in the session"),
            )
        })?;
        let snapshot = session.snapshot_cache.get(id).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SessionError,
                format!("The snapshot of step \"{label}\" is no longer cached"),
            )
        })?;
        subjects.push(Subject::new(label.clone(), snapshot.clone()));
    }
    Ok(interference_study(
        &subjects,
        session.precision,
        cancellation,
    ))
}

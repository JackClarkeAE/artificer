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

use crate::api::interference::{ClearanceState, FacetIndex, Placement, clearance};
use crate::{CancellationToken, NativeKernel, Snapshot};

/// The shape of the document this module publishes. A reader that
/// understands version `n` can refuse anything else outright.
pub const ANALYSIS_SCHEMA_VERSION: u32 = 1;

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
    /// `approximate` when any pair's measurement was, so a reader can tell
    /// at a glance whether the study rests on chords anywhere.
    pub tier: Tier,
    pub elapsed_ms: u64,
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
        tier,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
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

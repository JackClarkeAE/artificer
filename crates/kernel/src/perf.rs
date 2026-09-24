//! Stage timing for the kernel's own stages (ADR 0056, Track R).
//!
//! `perf_span!` records one metric per span into the compute pool's ring,
//! which the workbench's activity card reads; the ring holds a few hundred
//! entries, and a Boolean over a thousand faces runs its 2D stage a
//! thousand times. So every stage recorded here is also summed by name, and
//! [`take_stage_totals`] hands the sums to a bench or a test that wants a
//! breakdown of where an operation's time went.
//!
//! Everything here is dormant unless the crate is built with the
//! `perf-spans` feature and run with `ARTIFICER_PERF_REPORT` set, exactly
//! as `perf_span!` is: a shipped build pays nothing for it.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How often a stage ran and how long it took in all, since the totals were
/// last taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageTotal {
    pub calls: u64,
    pub items: u64,
    pub elapsed: Duration,
}

static TOTALS: Mutex<BTreeMap<&'static str, StageTotal>> = Mutex::new(BTreeMap::new());

/// Whether stages are being timed at all.
pub(crate) fn enabled() -> bool {
    artificer_compute::perf_spans_enabled()
}

/// Runs `work`, timing it as `task` over `items` when timing is on.
pub(crate) fn stage<T>(task: &'static str, items: usize, work: impl FnOnce() -> T) -> T {
    if !enabled() {
        return work();
    }
    let started = Instant::now();
    let value = work();
    record(task, items, started.elapsed());
    value
}

/// Records an already measured span, when timing is on.
pub(crate) fn record(task: &'static str, items: usize, elapsed: Duration) {
    if !enabled() {
        return;
    }
    artificer_compute::ComputePool::global().record_span(task, items, elapsed);
    let mut totals = TOTALS.lock().unwrap_or_else(PoisonError::into_inner);
    let total = totals.entry(task).or_default();
    total.calls += 1;
    total.items += items as u64;
    total.elapsed += elapsed;
}

/// Every stage total recorded since the last call, by name, and clears
/// them. Empty unless the crate was built with `perf-spans` and
/// `ARTIFICER_PERF_REPORT` is set.
#[doc(hidden)]
#[must_use]
pub fn take_stage_totals() -> Vec<(&'static str, StageTotal)> {
    let mut totals = TOTALS.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut *totals).into_iter().collect()
}

//! Deterministic parallel compute and cancellable priority jobs for Artificer.
//!
//! Indexed batches always publish in input order. Callers retain serial control
//! of topology allocation, canonical reductions, and transaction commits.

use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_PARALLEL_MIN_ITEMS: usize = 32;
const MAX_METRICS: usize = 512;

/// Whether [`perf_span!`] should time its body.
///
/// Optimising without instruments is guesswork, but instruments that cost
/// something in the default build are their own regression. The macro compiles
/// to nothing unless the `perf-spans` feature is on, and even then it stays
/// dormant until `ARTIFICER_PERF_REPORT` is set — so a development build can
/// switch profiling on without a rebuild, and a shipped build cannot pay for
/// it at all.
#[must_use]
pub fn perf_spans_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        cfg!(feature = "perf-spans")
            && std::env::var_os("ARTIFICER_PERF_REPORT").is_some_and(|value| value != "0")
    })
}

/// Times one serial stage into the global pool's metric ring.
///
/// `perf_span!("kernel.boolean.prism", faces, { ... })` evaluates the block and
/// returns its value either way; only the timing is conditional.
#[macro_export]
macro_rules! perf_span {
    ($task:literal, $items:expr, $body:block) => {{
        if $crate::perf_spans_enabled() {
            let items = $items;
            let started = std::time::Instant::now();
            let value = $body;
            $crate::ComputePool::global().record_span($task, items, started.elapsed());
            value
        } else {
            $body
        }
    }};
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Serial,
    Parallel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComputeConfig {
    pub threads: usize,
    pub parallel_min_items: usize,
}

impl ComputeConfig {
    #[must_use]
    pub fn available() -> Self {
        Self {
            threads: thread::available_parallelism().map_or(1, usize::from),
            parallel_min_items: DEFAULT_PARALLEL_MIN_ITEMS,
        }
    }

    #[must_use]
    pub const fn serial() -> Self {
        Self {
            threads: 1,
            parallel_min_items: usize::MAX,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComputeMetric {
    pub task: &'static str,
    pub mode: ExecutionMode,
    pub items: usize,
    pub elapsed: Duration,
}

#[derive(Clone)]
pub struct ComputePool {
    pool: Arc<rayon::ThreadPool>,
    config: ComputeConfig,
    metrics: Arc<Mutex<VecDeque<ComputeMetric>>>,
}

impl fmt::Debug for ComputePool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComputePool")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ComputePool {
    pub fn new(config: ComputeConfig) -> Result<Self, rayon::ThreadPoolBuildError> {
        let threads = config.threads.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("artificer-compute-{index}"))
            .build()?;
        Ok(Self {
            pool: Arc::new(pool),
            config: ComputeConfig { threads, ..config },
            metrics: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_METRICS))),
        })
    }

    #[must_use]
    pub fn global() -> &'static Self {
        static GLOBAL: OnceLock<ComputePool> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let mut config = ComputeConfig::available();
            if let Ok(value) = std::env::var("ARTIFICER_COMPUTE_THREADS")
                && let Ok(threads) = value.parse::<usize>()
            {
                config.threads = threads.max(1);
            }
            if let Ok(value) = std::env::var("ARTIFICER_PARALLEL_MIN_ITEMS")
                && let Ok(items) = value.parse::<usize>()
            {
                config.parallel_min_items = items.max(1);
            }
            Self::new(config).expect("the Artificer compute pool must be constructible")
        })
    }

    #[must_use]
    pub const fn config(&self) -> &ComputeConfig {
        &self.config
    }

    #[must_use]
    pub fn mode_for(&self, items: usize) -> ExecutionMode {
        if self.config.threads > 1 && items >= self.config.parallel_min_items {
            ExecutionMode::Parallel
        } else {
            ExecutionMode::Serial
        }
    }

    /// Maps an indexed batch in parallel while preserving its exact input order.
    pub fn map<T, U, F>(&self, task: &'static str, input: &[T], operation: F) -> Vec<U>
    where
        T: Sync,
        U: Send,
        F: Fn(usize, &T) -> U + Send + Sync,
    {
        self.map_with_workload(task, input, input.len(), operation)
    }

    /// Maps an indexed batch using a separate work estimate for the serial /
    /// parallel threshold. This is useful for a small number of expensive
    /// independent families, faces, or bodies.
    pub fn map_with_workload<T, U, F>(
        &self,
        task: &'static str,
        input: &[T],
        workload_items: usize,
        operation: F,
    ) -> Vec<U>
    where
        T: Sync,
        U: Send,
        F: Fn(usize, &T) -> U + Send + Sync,
    {
        let mode = self.mode_for(workload_items);
        let started = Instant::now();
        let output = match mode {
            ExecutionMode::Serial => input
                .iter()
                .enumerate()
                .map(|(index, value)| operation(index, value))
                .collect(),
            ExecutionMode::Parallel => self.pool.install(|| {
                input
                    .par_iter()
                    .enumerate()
                    .map(|(index, value)| operation(index, value))
                    .collect()
            }),
        };
        self.record(ComputeMetric {
            task,
            mode,
            items: workload_items,
            elapsed: started.elapsed(),
        });
        output
    }

    /// Produces ordered chunks in parallel, then flattens them serially.
    pub fn flat_map<T, U, F>(&self, task: &'static str, input: &[T], operation: F) -> Vec<U>
    where
        T: Sync,
        U: Send,
        F: Fn(usize, &T) -> Vec<U> + Send + Sync,
    {
        self.map(task, input, operation)
            .into_iter()
            .flatten()
            .collect()
    }

    pub fn flat_map_with_workload<T, U, F>(
        &self,
        task: &'static str,
        input: &[T],
        workload_items: usize,
        operation: F,
    ) -> Vec<U>
    where
        T: Sync,
        U: Send,
        F: Fn(usize, &T) -> Vec<U> + Send + Sync,
    {
        self.map_with_workload(task, input, workload_items, operation)
            .into_iter()
            .flatten()
            .collect()
    }

    #[must_use]
    pub fn recent_metrics(&self) -> Vec<ComputeMetric> {
        self.metrics
            .lock()
            .expect("metrics lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn clear_metrics(&self) {
        self.metrics.lock().expect("metrics lock poisoned").clear();
    }

    /// Records one externally timed span into the same ring buffer the
    /// parallel batches use, so the COMPUTE ACTIVITY card shows serial kernel
    /// stages beside the parallel ones instead of only the parallel ones.
    pub fn record_span(&self, task: &'static str, items: usize, elapsed: Duration) {
        self.record(ComputeMetric {
            task,
            mode: ExecutionMode::Serial,
            items,
            elapsed,
        });
    }

    fn record(&self, metric: ComputeMetric) {
        let mut metrics = self.metrics.lock().expect("metrics lock poisoned");
        if metrics.len() == MAX_METRICS {
            metrics.pop_front();
        }
        metrics.push_back(metric);
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum JobPriority {
    Background,
    Rebuild,
    Commit,
    InteractivePreview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobError {
    Cancelled,
    /// The job panicked. The worker caught the unwind so the process and the
    /// scheduler remain alive; callers should surface this as an internal
    /// operation failure with an incident identifier.
    Panicked,
    SchedulerStopped,
}

#[derive(Debug)]
pub struct JobHandle<T> {
    id: u64,
    cancellation: CancellationToken,
    receiver: mpsc::Receiver<Result<T, JobError>>,
}

impl<T> JobHandle<T> {
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn try_take(&self) -> Option<Result<T, JobError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(JobError::SchedulerStopped)),
        }
    }

    pub fn wait(self) -> Result<T, JobError> {
        self.receiver
            .recv()
            .unwrap_or(Err(JobError::SchedulerStopped))
    }
}

type JobAction = Box<dyn FnOnce() + Send + 'static>;

struct QueuedJob {
    priority: JobPriority,
    sequence: u64,
    action: JobAction,
}

impl PartialEq for QueuedJob {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl Eq for QueuedJob {}

impl PartialOrd for QueuedJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedJob {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

#[derive(Default)]
struct SchedulerState {
    queue: BinaryHeap<QueuedJob>,
    superseded: BTreeMap<String, CancellationToken>,
    stopped: bool,
}

struct SchedulerShared {
    state: Mutex<SchedulerState>,
    ready: Condvar,
}

struct SchedulerInner {
    shared: Arc<SchedulerShared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    sequence: AtomicU64,
}

impl Drop for SchedulerInner {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().expect("scheduler lock poisoned");
            state.stopped = true;
            for token in state.superseded.values() {
                token.cancel();
            }
            state.queue.clear();
        }
        self.shared.ready.notify_all();
        for worker in self
            .workers
            .get_mut()
            .expect("worker lock poisoned")
            .drain(..)
        {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub struct JobScheduler(Arc<SchedulerInner>);

impl fmt::Debug for JobScheduler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobScheduler")
            .finish_non_exhaustive()
    }
}

impl JobScheduler {
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let shared = Arc::new(SchedulerShared {
            state: Mutex::new(SchedulerState::default()),
            ready: Condvar::new(),
        });
        let workers = (0..worker_count.max(1))
            .map(|index| {
                let shared = Arc::clone(&shared);
                thread::Builder::new()
                    .name(format!("artificer-job-{index}"))
                    .spawn(move || worker_loop(&shared))
                    .expect("Artificer job worker must start")
            })
            .collect();
        Self(Arc::new(SchedulerInner {
            shared,
            workers: Mutex::new(workers),
            sequence: AtomicU64::new(0),
        }))
    }

    /// Submits a job. Reusing a supersede key cancels the older preview/job.
    pub fn submit<T, F>(
        &self,
        priority: JobPriority,
        supersede_key: Option<&str>,
        operation: F,
    ) -> JobHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(CancellationToken) -> T + Send + 'static,
    {
        let id = self.0.sequence.fetch_add(1, AtomicOrdering::Relaxed);
        let cancellation = CancellationToken::default();
        let (sender, receiver) = mpsc::channel();
        let job_cancellation = cancellation.clone();
        let action = Box::new(move || {
            if job_cancellation.is_cancelled() {
                let _ = sender.send(Err(JobError::Cancelled));
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                operation(job_cancellation.clone())
            }))
            .map_err(|_| JobError::Panicked)
            .and_then(|value| {
                if job_cancellation.is_cancelled() {
                    Err(JobError::Cancelled)
                } else {
                    Ok(value)
                }
            });
            let _ = sender.send(result);
        });
        let mut state = self.0.shared.state.lock().expect("scheduler lock poisoned");
        if let Some(key) = supersede_key
            && let Some(previous) = state
                .superseded
                .insert(key.to_owned(), cancellation.clone())
        {
            previous.cancel();
        }
        state.queue.push(QueuedJob {
            priority,
            sequence: id,
            action,
        });
        drop(state);
        self.0.shared.ready.notify_one();
        JobHandle {
            id,
            cancellation,
            receiver,
        }
    }
}

fn worker_loop(shared: &SchedulerShared) {
    loop {
        let action = {
            let mut state = shared.state.lock().expect("scheduler lock poisoned");
            while state.queue.is_empty() && !state.stopped {
                state = shared.ready.wait(state).expect("scheduler lock poisoned");
            }
            if state.stopped {
                return;
            }
            state.queue.pop().map(|job| job.action)
        };
        if let Some(action) = action {
            action();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_is_bit_for_bit_ordered_like_serial_map() {
        let input = (0_u64..20_000).collect::<Vec<_>>();
        let serial = ComputePool::new(ComputeConfig::serial()).unwrap();
        let parallel = ComputePool::new(ComputeConfig {
            threads: 4,
            parallel_min_items: 1,
        })
        .unwrap();
        let operation = |index: usize, value: &u64| {
            value
                .wrapping_mul(6364136223846793005)
                .rotate_left((index % 63) as u32)
        };
        assert_eq!(
            serial.map("determinism", &input, operation),
            parallel.map("determinism", &input, operation)
        );
    }

    #[test]
    fn a_new_superseding_job_cancels_the_old_result() {
        let scheduler = JobScheduler::new(1);
        let blocker = scheduler.submit(JobPriority::InteractivePreview, None, |_| {
            thread::sleep(Duration::from_millis(20));
        });
        let old = scheduler.submit(JobPriority::Background, Some("preview"), |_| 1_u8);
        let new = scheduler.submit(JobPriority::InteractivePreview, Some("preview"), |_| 2_u8);
        blocker.wait().unwrap();
        assert_eq!(new.wait(), Ok(2));
        assert_eq!(old.wait(), Err(JobError::Cancelled));
    }

    #[test]
    fn explicit_cancellation_prevents_publication() {
        let scheduler = JobScheduler::new(1);
        let blocker = scheduler.submit(JobPriority::Commit, None, |_| {
            thread::sleep(Duration::from_millis(20));
        });
        let cancelled = scheduler.submit(JobPriority::Background, None, |_| 7_u8);
        cancelled.cancel();
        blocker.wait().unwrap();
        assert_eq!(cancelled.wait(), Err(JobError::Cancelled));
    }

    #[test]
    fn a_panicking_job_is_contained_and_the_worker_remains_available() {
        let scheduler = JobScheduler::new(1);
        let panicked = scheduler.submit(JobPriority::Commit, None, |_| -> u8 {
            panic!("synthetic modeling failure")
        });
        assert_eq!(panicked.wait(), Err(JobError::Panicked));

        let healthy = scheduler.submit(JobPriority::Commit, None, |_| 42_u8);
        assert_eq!(healthy.wait(), Ok(42));
    }
}

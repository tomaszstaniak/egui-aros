//! A small fixed pool of worker threads for a GUI program.
//!
//! A frame must not wait: it hands a job to the pool, keeps the [`Job`] handle, and asks for the
//! result with [`Job::try_take`], which never blocks. When a result is ready the pool calls the
//! wake-up function it was given (in a GUI: ask for a repaint), so no frame has to poll.
//!
//! The pool is bounded on purpose. It has a fixed number of workers and a fixed number of waiting
//! jobs; one more is refused with [`SubmitError::Full`], and the caller decides what that means
//! (wait, report "busy"). No thread is created per job.
//!
//! Shutdown is orderly and does not block either: [`Executor::begin_shutdown`] refuses new jobs,
//! the jobs already accepted are still run (a save that was accepted is written), and
//! [`Executor::is_drained`] turns true when every worker has ended. Dropping the executor begins
//! the same shutdown without waiting. Nothing is ever interrupted by force.
//!
//! The crate knows nothing about windows, documents or the platform: jobs are plain closures and
//! their results plain values. A job's own failure belongs in its result type; [`JobError`] only
//! covers what the pool can observe. Where panics unwind, a panicking job is reported as
//! [`JobError::Panicked`] and the worker goes on; where they abort the process, they abort it.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::Poll;

/// The stack of a worker thread. Serializing a large document recurses deeper than the small
/// default stacks of some systems allow.
pub const WORKER_STACK: usize = 8 << 20;

/// What the pool calls when a job's result is ready.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

type Task = Box<dyn FnOnce() + Send + 'static>;

/// Identifies a job of one process: never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// The pool could not be started. No worker is left running.
#[derive(Debug)]
pub struct StartError {
    /// Workers that had started before the failure (they were told to end).
    pub started: usize,
    pub wanted: usize,
    pub reason: String,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the worker pool did not start ({} of {} workers): {}",
            self.started, self.wanted, self.reason
        )
    }
}

impl std::error::Error for StartError {}

/// Why a job was not accepted. It was not queued and will not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// As many jobs are waiting as the pool holds.
    Full,
    /// The pool is shutting down.
    Stopping,
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SubmitError::Full => "too many jobs are waiting",
            SubmitError::Stopping => "the worker pool is shutting down",
        })
    }
}

impl std::error::Error for SubmitError {}

/// Why a job has no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// The job panicked (only where panics unwind).
    Panicked,
    /// The job was accepted but never ran to its end (its worker is gone).
    Lost,
    /// The result was already taken.
    Taken,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JobError::Panicked => "the job failed with an internal error",
            JobError::Lost => "the job did not run to its end",
            JobError::Taken => "the job's result was already taken",
        })
    }
}

impl std::error::Error for JobError {}

/// The handle of an accepted job. Dropping it does not stop the job.
pub struct Job<T> {
    id: JobId,
    result: Option<Receiver<Result<T, JobError>>>,
}

impl<T> Job<T> {
    pub fn id(&self) -> JobId {
        self.id
    }

    /// The result if the job has finished; never waits. A result is handed out once.
    pub fn try_take(&mut self) -> Poll<Result<T, JobError>> {
        let Some(result) = &self.result else {
            return Poll::Ready(Err(JobError::Taken));
        };
        let taken = match result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Poll::Pending,
            Err(TryRecvError::Disconnected) => Err(JobError::Lost),
        };
        self.result = None;
        Poll::Ready(taken)
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<Task>,
    stopping: bool,
    /// Worker threads that have not ended.
    workers: usize,
}

struct Shared {
    state: Mutex<State>,
    work: Condvar,
    capacity: usize,
    wake: Wake,
}

impl Shared {
    /// A job never runs with the lock held, so a poisoned lock still guards consistent state.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn stop(&self) {
        self.lock().stopping = true;
        self.work.notify_all();
    }
}

/// Counts down a worker when its thread ends, however it ends.
struct WorkerEnds(Arc<Shared>);

impl Drop for WorkerEnds {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.workers = state.workers.saturating_sub(1);
    }
}

fn worker(shared: Arc<Shared>) {
    let _ends = WorkerEnds(shared.clone());
    loop {
        let task = {
            let mut state = shared.lock();
            loop {
                if let Some(task) = state.queue.pop_front() {
                    break Some(task);
                }
                if state.stopping {
                    break None;
                }
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        match task {
            Some(task) => task(),
            None => return,
        }
    }
}

/// A view of a pool that outlives it, for checking that its workers ended.
#[derive(Clone)]
pub struct Probe(Arc<Shared>);

impl Probe {
    /// Worker threads that have not ended.
    pub fn workers(&self) -> usize {
        self.0.lock().workers
    }
}

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

pub struct Executor {
    shared: Arc<Shared>,
}

impl Executor {
    /// A pool of `worker_count` threads that holds up to `queue_capacity` waiting jobs and calls
    /// `wake` (from a worker thread) after each result is ready. Either every worker starts or
    /// none is left running.
    pub fn new(
        worker_count: usize,
        queue_capacity: usize,
        wake: Wake,
    ) -> Result<Executor, StartError> {
        Self::with_spawner(worker_count, queue_capacity, wake, |name, body| {
            std::thread::Builder::new()
                .name(name)
                .stack_size(WORKER_STACK)
                .spawn(body)
                .map(drop)
        })
    }

    /// [`Executor::new`] with the thread creation supplied (`spawn(name, body)` starts a thread
    /// running `body`): for hosts that create threads their own way, and for tests.
    pub fn with_spawner(
        worker_count: usize,
        queue_capacity: usize,
        wake: Wake,
        mut spawn: impl FnMut(String, Task) -> std::io::Result<()>,
    ) -> Result<Executor, StartError> {
        if worker_count == 0 || queue_capacity == 0 {
            return Err(StartError {
                started: 0,
                wanted: worker_count,
                reason: "a pool needs at least one worker and room for one job".into(),
            });
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            work: Condvar::new(),
            capacity: queue_capacity,
            wake,
        });
        for index in 0..worker_count {
            shared.lock().workers += 1;
            let for_worker = shared.clone();
            if let Err(error) = spawn(
                format!("craft-jobs {}", index + 1),
                Box::new(move || worker(for_worker)),
            ) {
                // The thread was not created: it will never count itself down.
                {
                    let mut state = shared.lock();
                    state.workers = state.workers.saturating_sub(1);
                }
                shared.stop();
                return Err(StartError {
                    started: index,
                    wanted: worker_count,
                    reason: error.to_string(),
                });
            }
        }
        Ok(Executor { shared })
    }

    /// Queues `job`. It runs on a worker thread; its value comes back through the handle.
    pub fn submit<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<Job<T>, SubmitError> {
        let (send, result) = sync_channel::<Result<T, JobError>>(1);
        let wake = self.shared.wake.clone();
        let task: Task = Box::new(move || {
            let value = run(job);
            // The handle may be gone; the job has run all the same.
            let _ = send.send(value);
            // Dropped before the wake-up: the frame it causes must find the result, not "pending".
            drop(send);
            wake();
        });
        {
            let mut state = self.shared.lock();
            if state.stopping {
                return Err(SubmitError::Stopping);
            }
            if state.queue.len() >= self.shared.capacity {
                return Err(SubmitError::Full);
            }
            state.queue.push_back(task);
        }
        self.shared.work.notify_one();
        Ok(Job {
            id: JobId(NEXT_JOB.fetch_add(1, Ordering::Relaxed)),
            result: Some(result),
        })
    }

    /// Jobs accepted and not yet started.
    pub fn queued(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// Worker threads that have not ended.
    pub fn workers(&self) -> usize {
        self.shared.lock().workers
    }

    /// Refuses new jobs from now on. Jobs already accepted still run; then the workers end.
    /// Does not wait.
    pub fn begin_shutdown(&mut self) {
        self.shared.stop();
    }

    /// Whether the shutdown is complete: no job waiting and every worker ended. Does not wait.
    pub fn is_drained(&self) -> bool {
        let state = self.shared.lock();
        state.stopping && state.queue.is_empty() && state.workers == 0
    }

    /// A view that stays valid after the executor is dropped.
    pub fn probe(&self) -> Probe {
        Probe(self.shared.clone())
    }
}

impl Drop for Executor {
    /// Begins the shutdown and returns at once: the workers finish what was accepted and end.
    fn drop(&mut self) {
        self.shared.stop();
    }
}

#[cfg(panic = "unwind")]
fn run<T>(job: impl FnOnce() -> T) -> Result<T, JobError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).map_err(|_| JobError::Panicked)
}

#[cfg(not(panic = "unwind"))]
fn run<T>(job: impl FnOnce() -> T) -> Result<T, JobError> {
    Ok(job())
}

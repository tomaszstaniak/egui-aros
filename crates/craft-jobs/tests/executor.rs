//! The executor as a GUI loop uses it: nothing here waits on a job except through the
//! test's own barriers, as a frame must not.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use craft_jobs::{Executor, Job, JobError, StartError, SubmitError};

fn no_wake() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

/// Polls `job` the way a frame does until it has a result; fails after five seconds.
fn finish<T>(job: &mut Job<T>) -> Result<T, JobError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Poll::Ready(result) = job.try_take() {
            return result;
        }
        assert!(Instant::now() < deadline, "the job did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A job that waits for the test: returns (release, started).
fn gate() -> (
    Sender<()>,
    Receiver<()>,
    impl FnOnce() -> u32 + Send + 'static,
) {
    let (release, wait) = channel::<()>();
    let (tell, started) = channel::<()>();
    let job = move || {
        let _ = tell.send(());
        let _ = wait.recv();
        7
    };
    (release, started, job)
}

#[test]
fn a_result_is_taken_without_waiting_for_it() {
    let executor = Executor::new(1, 4, no_wake()).unwrap();
    let (release, started, job) = gate();
    let mut job = executor.submit(job).unwrap();
    started.recv().unwrap();
    // The worker is inside the job: a frame that asks gets "not yet" at once.
    let asked = Instant::now();
    assert!(matches!(job.try_take(), Poll::Pending));
    assert!(asked.elapsed() < Duration::from_millis(200));
    release.send(()).unwrap();
    assert_eq!(finish(&mut job), Ok(7));
    // A result is handed out once.
    assert!(matches!(job.try_take(), Poll::Ready(Err(JobError::Taken))));
}

#[test]
fn the_wake_up_comes_after_the_result_is_there() {
    let woken = Arc::new(AtomicUsize::new(0));
    let count = woken.clone();
    let executor = Executor::new(
        2,
        4,
        Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    let mut job = executor.submit(|| "done").unwrap();
    until("no wake-up", || woken.load(Ordering::SeqCst) == 1);
    // Woken means ready: the frame the wake-up causes finds the result.
    assert!(matches!(job.try_take(), Poll::Ready(Ok("done"))));
    assert_eq!(woken.load(Ordering::SeqCst), 1, "one wake-up per job");
}

#[test]
fn a_full_queue_refuses_and_keeps_what_it_has() {
    let executor = Executor::new(1, 2, no_wake()).unwrap();
    let (release, started, first) = gate();
    let mut running = executor.submit(first).unwrap();
    started.recv().unwrap();
    let mut queued: Vec<Job<u32>> = (0..2)
        .map(|i| executor.submit(move || i).unwrap())
        .collect();
    assert_eq!(executor.submit(|| 99u32).err(), Some(SubmitError::Full));
    assert_eq!(executor.queued(), 2);
    release.send(()).unwrap();
    assert_eq!(finish(&mut running), Ok(7));
    assert_eq!(finish(&mut queued[0]), Ok(0));
    assert_eq!(finish(&mut queued[1]), Ok(1));
    // Room again.
    assert_eq!(finish(&mut executor.submit(|| 5u32).unwrap()), Ok(5));
}

#[test]
fn jobs_keep_their_order_on_one_worker_and_have_distinct_ids() {
    let executor = Executor::new(1, 8, no_wake()).unwrap();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut jobs = Vec::new();
    for i in 0..6 {
        let order = order.clone();
        jobs.push(
            executor
                .submit(move || order.lock().unwrap().push(i))
                .unwrap(),
        );
    }
    for job in &mut jobs {
        finish(job).unwrap();
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3, 4, 5]);
    let mut ids: Vec<u64> = jobs.iter().map(|j| j.id().0).collect();
    ids.dedup();
    assert_eq!(ids.len(), 6);
}

#[test]
fn shutdown_refuses_new_jobs_finishes_the_accepted_ones_and_ends_the_workers() {
    let mut executor = Executor::new(2, 4, no_wake()).unwrap();
    let (release, started, first) = gate();
    let mut running = executor.submit(first).unwrap();
    started.recv().unwrap();
    let mut queued = executor.submit(|| 11u32).unwrap();
    executor.begin_shutdown();
    assert_eq!(executor.submit(|| 0u32).err(), Some(SubmitError::Stopping));
    // Not drained while a job runs; asking does not wait for it.
    let asked = Instant::now();
    assert!(!executor.is_drained());
    assert!(asked.elapsed() < Duration::from_millis(200));
    release.send(()).unwrap();
    // Work accepted before the shutdown is done, not dropped: a save in the queue is written.
    assert_eq!(finish(&mut running), Ok(7));
    assert_eq!(finish(&mut queued), Ok(11));
    until("the workers did not end", || executor.is_drained());
    assert_eq!(executor.workers(), 0);
}

#[test]
fn a_dropped_job_handle_does_not_stop_the_job_or_the_worker() {
    let executor = Executor::new(1, 4, no_wake()).unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let count = ran.clone();
    drop(executor.submit(move || {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    until("the job did not run", || ran.load(Ordering::SeqCst) == 1);
    assert_eq!(finish(&mut executor.submit(|| 3u32).unwrap()), Ok(3));
}

#[test]
fn dropping_the_executor_ends_its_workers_without_waiting() {
    let alive = Arc::new(AtomicUsize::new(0));
    let (release, started, first) = gate();
    let executor = Executor::new(1, 4, no_wake()).unwrap();
    let probe = executor.probe();
    let _running = executor.submit(first).unwrap();
    started.recv().unwrap();
    let dropped = Instant::now();
    drop(executor);
    assert!(
        dropped.elapsed() < Duration::from_millis(200),
        "drop must not wait for the running job"
    );
    assert_eq!(probe.workers(), 1);
    release.send(()).unwrap();
    until("the worker did not end", || probe.workers() == 0);
    let _ = alive;
}

#[cfg(panic = "unwind")]
#[test]
fn a_job_that_panics_reports_it_and_the_worker_goes_on() {
    let executor = Executor::new(1, 4, no_wake()).unwrap();
    let mut bad = executor.submit(|| -> u32 { panic!("job failed") }).unwrap();
    assert_eq!(finish(&mut bad), Err(JobError::Panicked));
    assert_eq!(finish(&mut executor.submit(|| 4u32).unwrap()), Ok(4));
}

#[test]
fn a_pool_that_cannot_start_every_worker_starts_none() {
    let started = Arc::new(AtomicUsize::new(0));
    let count = started.clone();
    // The third thread cannot be created.
    let result = Executor::with_spawner(4, 4, no_wake(), move |name, body| {
        if count.fetch_add(1, Ordering::SeqCst) == 2 {
            return Err(std::io::Error::other("no more threads"));
        }
        std::thread::Builder::new().name(name).spawn(body).map(drop)
    });
    let Err(StartError {
        started: n, wanted, ..
    }) = result
    else {
        panic!("the pool started")
    };
    assert_eq!((n, wanted), (2, 4));
}

#[test]
fn no_workers_or_no_queue_is_refused() {
    assert!(Executor::new(0, 4, no_wake()).is_err());
    assert!(Executor::new(1, 0, no_wake()).is_err());
}

//! Native check of `craft-jobs`, run from a Shell.
//!
//!   job-probe rounds     20 pools in a row, each with 2 workers and 6 jobs; every result is
//!                        taken by polling, every pool is drained; status 0
//!   job-probe full       a queue of 2 with a held worker refuses the third waiting job
//!   job-probe shutdown   shutdown begins while a worker is busy: new jobs are refused, the busy
//!                        job and the waiting one still finish, the workers end
//!   job-probe leave      the program returns from main while its pool is only dropped, not
//!                        drained: the system must still let the program end
//!
//! Each prints what it saw and the program's threads before and after (`pthread thread #N` on
//! AROS). A Shell's `Echo $RC` shows the status: 0 passed, 10 a check failed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use craft_jobs::{Executor, Job, SubmitError};

aros_startup::minimum_stack!(1 << 20);

fn threads() -> usize {
    aros_startup::task_names().iter().filter(|t| t.starts_with("pthread")).count()
}

/// Polls like a frame does; `None` after ten seconds.
fn take<T>(job: &mut Job<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Poll::Ready(result) = job.try_take() {
            return result.ok();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

fn drained(executor: &Executor) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if executor.is_drained() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

fn check(ok: bool, what: &str) -> bool {
    println!("{} {what}", if ok { "ok  " } else { "FAIL" });
    ok
}

fn rounds() -> bool {
    let woken = Arc::new(AtomicUsize::new(0));
    let mut ok = true;
    let mut sum = 0u64;
    for round in 0..20u64 {
        let count = woken.clone();
        let Ok(mut executor) = Executor::new(2, 8, Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        })) else {
            return check(false, "pool start");
        };
        let mut jobs: Vec<Job<u64>> = (0..6u64).filter_map(|i| executor.submit(move || round * 10 + i).ok()).collect();
        ok &= jobs.len() == 6;
        for job in &mut jobs {
            match take(job) {
                Some(v) => sum += v,
                None => ok = false,
            }
        }
        executor.begin_shutdown();
        ok &= drained(&executor);
    }
    // 20 rounds of (round*10 + 0..5): 6*10*(0+..+19) + 20*15.
    ok &= check(sum == 6 * 10 * 190 + 20 * 15, &format!("120 results, sum {sum}"));
    ok &= check(woken.load(Ordering::SeqCst) == 120, &format!("{} wake-ups", woken.load(Ordering::SeqCst)));
    ok
}

fn full() -> bool {
    let Ok(executor) = Executor::new(1, 2, Arc::new(|| {})) else { return check(false, "pool start") };
    let (release, wait) = channel::<()>();
    let (tell, started) = channel::<()>();
    let Ok(mut held) = executor.submit(move || {
        let _ = tell.send(());
        let _ = wait.recv();
        1u32
    }) else {
        return check(false, "first job");
    };
    let _ = started.recv();
    let mut waiting: Vec<Job<u32>> = (2..4u32).filter_map(|i| executor.submit(move || i).ok()).collect();
    let mut ok = check(waiting.len() == 2, "two jobs wait");
    ok &= check(executor.submit(|| 0u32).err() == Some(SubmitError::Full), "the third is refused");
    let _ = release.send(());
    ok &= check(take(&mut held) == Some(1), "the held job finished");
    ok &= check(waiting.iter_mut().map(take).collect::<Vec<_>>() == vec![Some(2), Some(3)], "the waiting jobs ran in order");
    ok
}

fn shutdown() -> bool {
    let Ok(mut executor) = Executor::new(2, 4, Arc::new(|| {})) else { return check(false, "pool start") };
    let (release, wait) = channel::<()>();
    let (tell, started) = channel::<()>();
    let Ok(mut busy) = executor.submit(move || {
        let _ = tell.send(());
        let _ = wait.recv();
        1u32
    }) else {
        return check(false, "first job");
    };
    let _ = started.recv();
    let Ok(mut waiting) = executor.submit(|| 2u32) else { return check(false, "second job") };
    executor.begin_shutdown();
    let mut ok = check(executor.submit(|| 0u32).err() == Some(SubmitError::Stopping), "new jobs are refused");
    let asked = Instant::now();
    ok &= check(!executor.is_drained() && asked.elapsed() < Duration::from_millis(500), "not drained while busy, asked without waiting");
    let _ = release.send(());
    ok &= check(take(&mut busy) == Some(1) && take(&mut waiting) == Some(2), "accepted jobs finished");
    ok &= check(drained(&executor), "the workers ended");
    ok
}

fn leave() -> bool {
    let Ok(executor) = Executor::new(2, 4, Arc::new(|| {})) else { return check(false, "pool start") };
    let mut job = match executor.submit(|| 5u32) {
        Ok(job) => job,
        Err(_) => return check(false, "job"),
    };
    let ok = check(take(&mut job) == Some(5), "a job ran");
    // Dropped, not drained: the workers are told to end and main returns right away.
    drop(executor);
    ok
}

fn main() {
    let action = std::env::args().nth(1).unwrap_or_default();
    println!("job-probe {action}: {} thread(s) before", threads());
    let ok = match action.as_str() {
        "rounds" => rounds(),
        "full" => full(),
        "shutdown" => shutdown(),
        "leave" => leave(),
        _ => {
            eprintln!("usage: job-probe rounds|full|shutdown|leave");
            std::process::exit(20);
        }
    };
    println!("job-probe {action}: {} thread(s) after", threads());
    std::process::exit(if ok { 0 } else { 10 });
}

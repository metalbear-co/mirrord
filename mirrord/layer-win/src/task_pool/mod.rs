//! Generic background thread pool for the Windows layer.
//!
//! A small fixed pool of worker threads consuming erased closures off a
//! shared channel. It is deliberately ignorant of *what* the closures do:
//! the IOCP async-read path ([`crate::iocp`]) submits a closure that does the
//! agent round-trip and posts a completion packet; the async DNS path
//! ([`crate::hooks::socket`]) submits a closure that resolves through the
//! proxy and then fires the caller's `GetAddrInfoExW` completion. Both just
//! need "run this `FnOnce` on a thread that isn't the caller's".
//!
//! ## Pool sizing
//!
//! [`WORKER_COUNT`] is a small fixed number consuming jobs from a shared
//! `std::sync::mpsc::Receiver` under a `Mutex` (the receiver isn't `Sync`, so
//! workers serialize on `rx.lock()`; the mutex is held only for the `recv()`
//! call, not while the job runs).
//!
//! - **`PROXY_CONNECTION` serializes** at the layer-lib level: only one worker can talk to the
//!   agent at a time, so a large pool wouldn't increase agent throughput.
//! - **But** workers also do local work (buffer copies, chain building, lock juggling) outside the
//!   proxy mutex; 4 workers comfortably absorb bursts.
//! - **Worker threads are cheap to keep alive** (they `recv()` and park), and avoiding per-call
//!   `thread::spawn` matters for clients like the .NET TPL or Node libuv that fire many concurrent
//!   operations.
//!
//! ## Initialization
//!
//! The pool is created on first use, but [`initialize`] is called eagerly at
//! layer boot so the worker threads are spawned from a normal layer thread —
//! never lazily from inside a hook that might be running under the loader
//! lock (e.g. a DNS resolution during another DLL's `DllMain`), where
//! `thread::spawn` would deadlock on `DLL_THREAD_ATTACH`. Workers run for the
//! layer's lifetime; we don't join them on `DLL_PROCESS_DETACH` (the process
//! is dying, John.).
//!
//! ## Re-entrancy / deadlock note
//!
//! A submitted job must never submit-*and-wait-on* another job: with a fixed
//! pool, a closure that blocks until a second submission completes can pin all
//! workers. Both current callers are fire-and-forget (they signal their own
//! completion mechanism and return), so a worker never waits on the pool.

use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use once_cell::sync::Lazy;

/// Fixed pool size. Nothing particularly prevents you from setting it to
/// `10_000`, perhaps it would even be enriching. See module doc for rationale.
const WORKER_COUNT: usize = 4;

/// Erased job. `Box<dyn FnOnce>` because each submission has its own captured
/// state (raw pointers cast to usize, agent fd, completion context, etc.).
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Lazily-initialized sender; the receiver is shared across all worker threads
/// via `Arc<Mutex<Receiver<Job>>>` so the first worker to wake up grabs the
/// next job. (Stock `mpsc::Receiver` isn't `Sync`, hence the mutex; the mutex
/// is held only for the few nanoseconds of `recv`, not while the job runs.)
static POOL: Lazy<Sender<Job>> = Lazy::new(|| {
    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for id in 0..WORKER_COUNT {
        let rx = Arc::clone(&rx);
        thread::Builder::new()
            .name(format!("mirrord-task-worker-{id}"))
            .spawn(move || worker_loop(rx))
            .expect("failed to spawn task-pool worker thread");
    }
    tracing::info!("task-pool worker pool started ({} threads)", WORKER_COUNT);
    tx
});

/// Eagerly spawn the worker threads from a safe context (layer boot), so the
/// first [`submit`] from inside a hook never has to `thread::spawn` under the
/// loader lock. Idempotent.
pub(crate) fn initialize() {
    Lazy::force(&POOL);
}

fn worker_loop(rx: Arc<Mutex<Receiver<Job>>>) {
    // are you a named thread or just another thread Andy?
    let tid = std::thread::current()
        .name()
        .map(str::to_owned)
        .unwrap_or_else(|| "<unnamed>".to_owned());

    loop {
        // Lock the receiver only long enough to grab the next job; the mutex
        // is released before the job runs so other workers can pull in
        // parallel.
        let job = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(e) => {
                    // Poisoned guard means a worker panicked while holding the
                    // receiver. Shouldn't happen, we only hold during recv()
                    // which is panic-free, or the sender side panicked. Either
                    // way, no recovery.
                    tracing::error!(
                        worker = %tid,
                        error = ?e,
                        "task-pool worker: receiver mutex poisoned, worker exiting"
                    );
                    return;
                }
            };
            match guard.recv() {
                Ok(j) => j,
                Err(_) => {
                    // Sender side dropped: POOL is a Lazy static so this only
                    // fires at process teardown. Quiet exit.
                    tracing::debug!(worker = %tid, "task-pool worker: channel closed, exiting");
                    return;
                }
            }
        };
        tracing::trace!(worker = %tid, "task-pool worker: dequeued job");
        // Catch panics so one bad job doesn't take down the worker. Callers
        // that rely on a job signaling a completion (IOCP packet, DNS callback)
        // must install their own guaranteed-fire guard inside the closure; the
        // pool only guarantees the *worker* survives.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
            Ok(()) => tracing::trace!(worker = %tid, "task-pool worker: job done"),
            Err(panic) => {
                // Best-effort downcast to extract a useful message.
                // NOTE(gabriela): this is all claude's credit, i would've
                // never even thought of implementing this because i'd have
                // no idea how
                let msg = panic
                    .downcast_ref::<&'static str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("<non-string panic>");
                tracing::error!(
                    worker = %tid,
                    panic = msg,
                    "task-pool worker: job panicked; any completion the job was responsible for signaling may be lost unless the job installed a fire-on-drop guard"
                );
            }
        }
    }
}

/// Submit `f` to run on a worker thread. The closure must be `'static + Send`
/// because it crosses thread boundaries; the worker calls it once then loops
/// for the next job.
///
/// ## Pointer contract
///
/// Callers that need to pass raw pointers (a user-provided buffer / IOSB /
/// OVERLAPPED) MUST pre-cast them to `usize` inside the captured environment,
/// since `*mut T` is `!Send` and the borrow checker rightly rejects naive
/// captures.
///
/// Keeping those pointers valid until the job runs is the caller's
/// responsibility — the same contract the OS places on the originator of the
/// async operation.
pub(crate) fn submit<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    if let Err(e) = POOL.send(Box::new(f)) {
        // Channel send only fails when every worker has died, which is a bug.
        tracing::error!(
            error = ?e,
            "task_pool::submit: pool send failed; whatever completion this job would have signaled will never fire"
        );
    } else {
        tracing::trace!("task_pool::submit: queued job");
    }
}

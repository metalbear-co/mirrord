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
//! workers serialize on `rx.lock()`; one idle worker holds the mutex while it
//! blocks in `recv()`, and releases it before the job runs).
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
//! Two steps, and neither is left to the first [`submit`].
//!
//! - [`prepare`] creates the queue. `DllMain` calls it before it enables the hooks, so a hook
//!   always has somewhere to put a job. It only allocates.
//! - [`start_workers`] spawns the workers, from the layer's startup worker thread. Never from
//!   `DllMain`: a thread spawned there cannot start until the loader lock is released, and if the
//!   layer's startup fails there, nothing of it may run afterwards.
//!
//! [`submit`] never spawns a thread, because a hook can run under the loader lock (a DNS
//! resolution during another DLL's `DllMain`), where a spawn deadlocks on `DLL_THREAD_ATTACH`. A
//! job submitted before the workers exist waits in the queue. Workers run for the layer's
//! lifetime; we don't join them on `DLL_PROCESS_DETACH` (the process is dying, John.).
//!
//! ## Re-entrancy / deadlock note
//!
//! A submitted job must never submit-*and-wait-on* another job: with a fixed
//! pool, a closure that blocks until a second submission completes can pin all
//! workers. Both current callers are fire-and-forget (they signal their own
//! completion mechanism and return), so a worker never waits on the pool.

use std::{
    io,
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

/// A job queue and the workers that drain it.
///
/// The receiver is shared across all worker threads via `Arc<Mutex<Receiver<Job>>>` so the first
/// worker to wake up grabs the next job. (Stock `mpsc::Receiver` isn't `Sync`, hence the mutex.
/// The worker that holds it blocks in `recv` while the queue is empty; the others wait for the
/// mutex. No worker holds it while a job runs.)
struct TaskPool {
    sender: Sender<Job>,
    receiver: Arc<Mutex<Receiver<Job>>>,
}

impl TaskPool {
    /// Creates the queue, with no workers yet.
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        Self {
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    /// Spawns the workers. Called once, by the layer's startup worker.
    ///
    /// # Errors
    ///
    /// When a worker could not be spawned. The workers spawned before it keep running.
    fn start_workers(&self) -> io::Result<()> {
        // See the warning in `layer-lib::logging`.
        for id in 0..WORKER_COUNT {
            let receiver = Arc::clone(&self.receiver);
            thread::Builder::new()
                .name(format!("mirrord-task-worker-{id}"))
                .spawn(move || worker_loop(receiver))?;
        }
        tracing::info!("task-pool worker pool started ({} threads)", WORKER_COUNT);
        Ok(())
    }

    /// Queues `job` for a worker. Never spawns a thread.
    fn submit(&self, job: Job) {
        // The pool owns the receiver, so the channel stays open and the send cannot fail.
        let _ = self.sender.send(job);
        tracing::trace!("task_pool::submit: queued job");
    }
}

/// The layer's one pool. See the module doc for when each step runs.
static POOL: Lazy<TaskPool> = Lazy::new(TaskPool::new);

/// Creates the job queue, without spawning anything. Safe under the loader lock. Idempotent.
pub(crate) fn prepare() {
    Lazy::force(&POOL);
}

/// Spawns the worker threads. Must not run under the loader lock. Called once.
///
/// # Errors
///
/// When a worker could not be spawned.
pub(crate) fn start_workers() -> io::Result<()> {
    POOL.start_workers()
}

fn worker_loop(rx: Arc<Mutex<Receiver<Job>>>) {
    // Deliberately NOT marked internal. A job here can call back into application code -
    // `addrinfo_ex`'s `deliver` runs the caller's completion routine on this thread, and .NET
    // calls `FreeAddrInfoExW` from it. A marked thread bypasses that hook, so `ws2_32` frees a
    // chain this layer allocated and leaves a stale `MANAGED_ADDRINFO` entry, which the next
    // chain on that reused address turns into a double free (`0xC0000374`).
    //
    // Marking buys nothing anyway: the agent round-trip uses `send`/`recv`, which this layer
    // does not hook, on a socket created before any job runs.

    // are you a named thread or just another thread Andy?
    //
    // Safe here, unlike in a hook: this is a Rust-spawned thread. See the warning in
    // `layer-lib::logging`.
    #[allow(clippy::disallowed_methods)]
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
                tracing::error!(
                    worker = %tid,
                    panic = crate::panic_message(&*panic),
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
///
/// Never spawns a thread. A job submitted before [`start_workers`] waits in the queue.
pub(crate) fn submit<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    POOL.submit(Box::new(f));
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc::RecvTimeoutError, time::Duration};

    use super::*;

    /// Submitting spawns nothing: a job waits for the workers, and runs once they start.
    #[test]
    fn a_job_waits_for_the_workers() {
        let pool = TaskPool::new();
        let (done, finished) = mpsc::channel();

        pool.submit(Box::new(move || {
            let _ = done.send(());
        }));
        assert_eq!(
            finished.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout),
            "no worker exists to run it yet"
        );

        pool.start_workers().expect("start the workers");
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("a worker runs the queued job");
    }
}

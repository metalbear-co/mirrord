//! Background execution for async reads on managed file handles.
//!
//! The FS read hook (`hooks/files/mod.rs::nt_read_file_hook`) calls
//! [`submit`] when it sees a read on a managed file that's bound to a
//! port. The closure runs on a pre-spawned worker (not a fresh thread
//! per call), performs the synchronous agent round-trip (gated by the
//! singleton `PROXY_CONNECTION` mutex), writes the result into the
//! caller-provided buffer + IOSB, and posts a completion packet to the
//! bound OS port via [`super::enqueue_packet`]. Meanwhile the caller's
//! thread is freed to return `STATUS_PENDING` and drive its own event
//! loop.
//!
//! ## Pool sizing
//!
//! [`WORKER_COUNT`] is a small fixed number consuming jobs from a
//! shared `std::sync::mpsc::Receiver` under a `Mutex` (the receiver
//! isn't `Sync`, so workers serialize on `rx.lock()`; the mutex is
//! held only for the `recv()` call, not while the job runs).
//!
//! - **PROXY_CONNECTION serializes** at the layer-lib level: only one worker can talk to the agent
//!   at a time, so a large pool wouldn't increase throughput.
//! - **But** workers also do local work (buffer copies, lock juggling) outside the proxy mutex; 4
//!   workers comfortably absorbs bursts.
//! - **Worker threads are cheap to keep alive** (they `recv()` and park), and avoiding per-call
//!   `thread::spawn` matters for clients like the .NET TPL or Node libuv that fire many concurrent
//!   reads.
//!
//! The pool starts lazily on first [`submit`]. Processes that never
//! touch managed async file IO pay nothing. Workers run for the layer's
//! lifetime; we don't join them on `DLL_PROCESS_DETACH` (the process
//! is dying, John.).

use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use once_cell::sync::Lazy;

/// Fixed pool size. Nothing particularly prevents you from setting it
/// to `10_000`, perhaps it would even be enriching. See module doc
/// for rationale.
const WORKER_COUNT: usize = 4;

/// Erased job. `Box<dyn FnOnce>` because each submission has its own
/// captured state (raw buffer/IOSB pointers cast to usize, agent fd,
/// completion key, etc.).
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Lazily-initialized sender; the receiver is shared across all worker
/// threads via `Arc<Mutex<Receiver<Job>>>` so the first worker to wake
/// up grabs the next job. (Stock `mpsc::Receiver` isn't `Sync`, hence
/// the mutex; the mutex is held only for the few nanoseconds of `recv`,
/// not while the job runs.)
static POOL: Lazy<Sender<Job>> = Lazy::new(|| {
    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for id in 0..WORKER_COUNT {
        let rx = Arc::clone(&rx);
        thread::Builder::new()
            .name(format!("mirrord-iocp-worker-{id}"))
            .spawn(move || worker_loop(rx))
            .expect("failed to spawn IOCP worker thread");
    }
    tracing::info!("IOCP worker pool started ({} threads)", WORKER_COUNT);
    tx
});

fn worker_loop(rx: Arc<Mutex<Receiver<Job>>>) {
    // are you a named thread or just another thread Andy?
    let tid = std::thread::current()
        .name()
        .map(str::to_owned)
        .unwrap_or_else(|| "<unnamed>".to_owned());

    loop {
        // Lock the receiver only long enough to grab the next job; the
        // mutex is released before the job runs so other workers can
        // pull in parallel.
        let job = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(e) => {
                    // Poisoned guard means a worker panicked while
                    // holding the receiver. Shouldn't happen, we only
                    // hold during recv() which is panic-free, or the
                    // sender side panicked. Either way, no recovery.
                    tracing::error!(
                        worker = %tid,
                        error = ?e,
                        "iocp worker: receiver mutex poisoned, worker exiting"
                    );
                    return;
                }
            };
            match guard.recv() {
                Ok(j) => j,
                Err(_) => {
                    // Sender side dropped: POOL is a Lazy static so this
                    // only fires at process teardown. Quiet exit.
                    tracing::debug!(worker = %tid, "iocp worker: channel closed, exiting");
                    return;
                }
            }
        };
        tracing::trace!(worker = %tid, "iocp worker: dequeued job");
        // Catch panics so one bad job doesn't take down the worker
        // (and the user app with it, since the dequeue side would
        // wait forever for a packet from a thread that no longer
        // exists).
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
            Ok(()) => tracing::trace!(worker = %tid, "iocp worker: job done"),
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
                    "iocp worker: job panicked, caller's NtReadFile will appear to hang since no completion packet will be enqueued"
                );
            }
        }
    }
}

/// Submit `f` to run on a worker thread. The closure must be `'static +
/// Send` because it crosses thread boundaries; the worker calls it once
/// then loops for the next job.
///
/// **Pointer contract**: callers that need to pass raw pointers (a
/// user-provided buffer / IOSB) MUST pre-cast them to `usize` inside
/// the captured environment, since `*mut T` is `!Send` and the
/// borrow checker rightly rejects naive captures. The NT API
/// contract puts the burden on the *caller* of `NtReadFile` to keep
/// those buffers alive until the completion packet arrives, so the
/// layer is doing no worse than the OS would.
pub(crate) fn submit<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    if let Err(e) = POOL.send(Box::new(f)) {
        // Channel send only fails when every worker has died, which is a
        // bug. Log and drop: the user's `NtReadFile` will appear to
        // hang on the dequeue (the OS does the same when its own thread
        // pool fails).
        tracing::error!(
            error = ?e,
            "iocp::worker::submit: pool send failed, the caller's IO will hang waiting for a completion packet that no one will enqueue"
        );
    } else {
        tracing::trace!("iocp::worker::submit: queued job");
    }
}

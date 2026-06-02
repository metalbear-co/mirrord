//! IO completion port (IOCP) plumbing for managed file handles.
//!
//! The OS owns every IOCP end-to-end -- this module installs no
//! syscall hooks of its own. When a managed file's async `NtReadFile`
//! completes on a worker thread, we **post a completion packet to the
//! user's port** via the original `NtSetIoCompletion`; the OS handles
//! FIFO ordering, multi-thread fairness, timeout math, depth queries,
//! `Alertable=TRUE`, the per-port event, etc.
//!
//! ## Module layout
//!
//! - [`binding`] — file <-> (port, key) map populated by
//!   `NtSetInformationFile(FileCompletionInformation)`; queried by the FS read hook to decide
//!   whether to take the async path.
//! - [`packet`] — the [`IocpPacket`] struct and [`enqueue_packet`], which posts to the OS port via
//!   the captured original `NtSetIoCompletion`.
//!
//! The deferred closures run on the shared [`crate::task_pool`]. The FS hook
//! submits a closure there that does the agent IO and then calls
//! [`enqueue_packet`]; the caller's thread returns `STATUS_PENDING` immediately.
//!
//! ## Wire-up
//!
//! [`initialize`] runs once during layer boot. It resolves
//! `ntdll!NtSetIoCompletion` via `GetProcAddress` and stashes the
//! pointer in [`packet::ORIGINAL_NT_SET_IO_COMPLETION`] so the worker
//! can post without paying for a hook (and without any risk of
//! recursion, since no hook is installed).

pub(crate) mod binding;
pub(crate) mod packet;

pub(crate) use binding::{bind_file_to_port, binding_for_file, unbind_file};
use mirrord_layer_lib::LayerResult;
pub(crate) use packet::{IocpPacket, enqueue_packet};

use self::packet::{NtSetIoCompletionFn, ORIGINAL_NT_SET_IO_COMPLETION};

/// Resolve and cache `ntdll!NtSetIoCompletion`. Called once from
/// `hooks::initialize_hooks` at layer startup. Eager initialization
/// (vs `OnceLock::get_or_init` on first packet enqueue) makes a missing
/// export surface as a loud log line at boot rather than as a silent
/// IO hang later.
pub(crate) fn initialize() -> LayerResult<()> {
    let ptr = crate::process::get_export("ntdll", "NtSetIoCompletion");
    if ptr.is_null() {
        tracing::error!(
            "iocp::initialize: GetProcAddress(\"ntdll\", \"NtSetIoCompletion\") returned null -- \
             async file reads will not deliver completion packets, callers will hang"
        );
        return Ok(());
    }
    // SAFETY: `ptr` is the address of an unsafe extern "system" function
    // exported by ntdll. Transmuting a `*mut c_void` directly to a
    // function pointer of the matching ABI is the standard FFI idiom.
    let func: NtSetIoCompletionFn = unsafe { std::mem::transmute(ptr) };
    let _ = ORIGINAL_NT_SET_IO_COMPLETION.set(func);
    tracing::info!(
        addr = ?ptr,
        "iocp::initialize: captured ntdll!NtSetIoCompletion for async-read completion posting"
    );
    Ok(())
}

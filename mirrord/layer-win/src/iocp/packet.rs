//! Completion packets and the enqueue API.
//!
//! Each [`IocpPacket`] mirrors the on-wire
//! `FILE_IO_COMPLETION_INFORMATION` struct that
//! `NtRemoveIoCompletionEx` hands back to dequeue threads.
//!
//! [`enqueue_packet`] posts a packet directly to the **real OS port**
//! the user passed in `FileCompletionInformation`, via the original
//! `NtSetIoCompletion` (resolved through `GetProcAddress` in
//! [`super::initialize`]).
//!
//! The OS handles FIFO ordering, the per-port auto-reset event,
//! multi-thread fairness, timeout math, and `Depth` reporting; we just
//! call the syscall.

use std::sync::OnceLock;

use winapi::shared::{
    basetsd::ULONG_PTR,
    ntdef::{HANDLE, NTSTATUS, PVOID},
};

/// One queued completion. Field names mirror
/// `FILE_IO_COMPLETION_INFORMATION` / the args of `NtSetIoCompletion`:
///
/// | here          | NT API                       |
/// |---------------|------------------------------|
/// | `key`         | `KeyContext` / `CompletionKey` (from `FileCompletionInformation` at bind time) |
/// | `apc_context` | `ApcContext` (verbatim from `NtReadFile`; the OVERLAPPED* slot) |
/// | `status`      | `IoStatusBlock.Status`       |
/// | `information` | `IoStatusBlock.Information` (bytes transferred) |
pub(crate) struct IocpPacket {
    pub(crate) key: usize,
    pub(crate) apc_context: usize,
    pub(crate) status: NTSTATUS,
    pub(crate) information: usize,
}

/// Function-pointer type matching the real `NtSetIoCompletion`.
/// Populated by [`super::initialize`] via `GetProcAddress`.
pub(crate) type NtSetIoCompletionFn =
    unsafe extern "system" fn(HANDLE, PVOID, PVOID, NTSTATUS, ULONG_PTR) -> NTSTATUS;

/// Set once during layer init. [`enqueue_packet`] calls before
/// initialization log an error and drop. Harmless in practice since
/// no managed file IO is possible before init runs.
pub(crate) static ORIGINAL_NT_SET_IO_COMPLETION: OnceLock<NtSetIoCompletionFn> = OnceLock::new();

/// Post `packet` to `port` by calling the original `NtSetIoCompletion`.
/// The OS does FIFO ordering, signaling, multi-thread fairness, etc.
/// We just translate our `IocpPacket` fields to the raw arguments.
///
/// Silent (with a warn log) if the original isn't set yet, which only
/// happens if something tries to enqueue before hook init (a bug
/// worth flagging but not panicking over).
///
/// `port` is a raw `HANDLE` (`*mut c_void`) but we never dereference
/// it, the OS does. Clippy can't tell, so we do some practical
/// brainwashing so that it approves.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub(crate) fn enqueue_packet(port: HANDLE, packet: IocpPacket) {
    let Some(&original) = ORIGINAL_NT_SET_IO_COMPLETION.get() else {
        tracing::error!(
            ?port,
            "iocp::enqueue_packet: ORIGINAL_NT_SET_IO_COMPLETION not initialized, \
             hook init didn't run before async IO; the caller's NtReadFile will appear to hang"
        );
        return;
    };
    // SAFETY: `port` is a real OS handle (the user passed it to us via
    // `FileCompletionInformation` at bind time). `original` is the
    // unhooked syscall pointer. The remaining args are raw values we
    // round-trip from the IocpPacket fields, no Rust references or
    // ownership are involved.
    let status = unsafe {
        original(
            port,
            packet.key as PVOID,
            packet.apc_context as PVOID,
            packet.status,
            packet.information as ULONG_PTR,
        )
    };
    if status < 0 {
        tracing::error!(
            ?port,
            ntstatus = format!("0x{:08X}", status as u32),
            key = packet.key,
            apc_context = packet.apc_context,
            "iocp::enqueue_packet: NtSetIoCompletion failed, packet lost, caller will hang"
        );
    } else {
        tracing::debug!(
            ?port,
            key = packet.key,
            apc_context = packet.apc_context,
            status = packet.status,
            information = packet.information,
            "iocp::enqueue_packet: posted completion to OS port"
        );
    }
}

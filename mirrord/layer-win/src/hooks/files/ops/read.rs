//! Body of `nt_read_file_hook` and its async worker. The hook in the
//! [FS-hook dispatcher](super::super) is a thin delegate into
//! [`handle`].
//!
//! [`handle`] reads from a managed file by routing through the agent.
//! For unmanaged file handles it falls through to the original
//! `NtReadFile`.
//!
//! After locking the
//! [`crate::hooks::files::managed_handle::HandleContext`]
//! and validating `buffer` + `io_status_block` (invalid pointer →
//! `STATUS_ACCESS_VIOLATION`), the hook calls [`classify_read`] to
//! decide which of the [`ReadStrategy`] variants applies, then
//! dispatches to a focused helper for that variant. Splitting the
//! "decide what to do" code from "do it" code keeps each helper
//! readable in isolation and makes the dispatch table itself easy to
//! audit.
//!
//! The strategy variants:
//!
//! ### `length == 0`
//!
//! Short-circuits to [`STATUS_SUCCESS`] with `Information = 0`; the
//! buffer is untouched. If the file is bound to a port, a zero-byte
//! completion packet is still queued (the kernel does too: the
//! operation counts as an IO). Matches `Test_ZeroLengthRead`.
//!
//! ### Async open-mode validation (no `FILE_SYNCHRONOUS_IO_*`)
//!
//! Async handles **must** provide a non-NULL `byte_offset` and must not
//! use the `FILE_USE_FILE_POINTER_POSITION` (-2) sentinel; both are
//! rejected with [`STATUS_INVALID_PARAMETER`] independently of port
//! binding. Matches `Test_NullOffsetRejected`,
//! `Test_SentinelOffsetRejected`, `Test_SynchronizeAccessWithoutSyncFlag`.
//!
//! ### Async-bound path (file has an IOCP binding from `NtSetInformationFile(FileCompletionInformation)`)
//!
//! - If `skip_on_success` is set on the file (via `FileIoCompletionNotificationInformation`): do
//!   the read inline on the caller's thread. On `STATUS_SUCCESS`, return immediately with no packet
//!   queued (the optimization clients like .NET enable). On any other status, queue a packet and
//!   return that status -- the flag only covers success.
//! - Otherwise: capture everything the worker needs (fd, path, buffer pointer-as-usize, IOSB
//!   pointer-as-usize, port handle, key, ApcContext, length, offset), drop the registry locks, and
//!   submit the read to [`iocp::worker::submit`]. Return [`STATUS_PENDING`] immediately: the
//!   caller's thread is freed; the worker writes the buffer + IOSB and posts a packet to the OS
//!   port via the original `NtSetIoCompletion` when the agent round-trip finishes.
//!
//! ### Sync/inline path (no port binding: sync handle or unbound async)
//!
//! Direct in-thread agent round-trip:
//!
//! - `byte_offset == NULL`: agent cursor IS the stored position; the read uses & advances it
//!   naturally (no pre- or post-read seek).
//! - `byte_offset` explicit: peek the cursor, seek to the offset, read, restore the cursor -- the
//!   NT contract is "explicit offset on sync handles doesn't disturb the stored position." Matches
//!   `sync::Test_StoredPositionHonoured` / `sync::Test_SetPositionRespected`.
//!
//! Sends [`ReadFileRequest`] for `length` bytes; an empty response
//! means EOF ([`STATUS_END_OF_FILE`], `Information = 0`); otherwise
//! copies `min(bytes.len(), length)` bytes into `buffer`, records the
//! count in `io_status_block.Information`, returns [`STATUS_SUCCESS`].
//!
//! Any agent failure surfaces as [`STATUS_UNEXPECTED_NETWORK_ERROR`].
//!
//! ## Workers
//!
//! [`perform_async_read`] is the actual agent round-trip for an async
//! read; returns the `(status, bytes_copied)` pair the caller writes
//! into the IOSB.
//!
//! [`do_async_read`] is the worker routine run by [`iocp::worker`]:
//! save/seek/read/restore-cursor against the agent, write the
//! user-provided buffer + IOSB, then post a completion packet to the
//! bound port. `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` is handled
//! **inline in the main thread before submit** (the inline-success
//! path returns before `submit` is ever called), so when this worker
//! does run it always posts a packet -- once we've deferred, the
//! user is waiting for it.

use std::ffi::c_void;

use mirrord_layer_lib::proxy_connection::make_proxy_request_with_response;
use mirrord_protocol::file::{ReadFileRequest, SeekFromInternal};
use phnt::ffi::{_IO_STATUS_BLOCK, FILE_SYNCHRONOUS_IO_ALERT, FILE_SYNCHRONOUS_IO_NONALERT};
use winapi::shared::{
    ntdef::{HANDLE, NTSTATUS, PLARGE_INTEGER, PULONG, PVOID, ULONG},
    ntstatus::{
        STATUS_END_OF_FILE, STATUS_INVALID_PARAMETER, STATUS_PENDING, STATUS_SUCCESS,
        STATUS_UNEXPECTED_NETWORK_ERROR,
    },
};

use crate::{
    hooks::files::{
        iosb::{check_io_pointers, write_iosb, write_iosb_eof, write_iosb_success},
        managed_handle::{HandleContext, MANAGED_HANDLES},
        types::NT_READ_FILE_ORIGINAL,
        util::{WindowsTime, try_seek},
    },
    iocp,
};

/// `FILE_USE_FILE_POINTER_POSITION`. NT's convention for "use the
/// stored position" when passed as the `ByteOffset` of a sync read;
/// async handles must instead provide an explicit offset, so the
/// layer rejects this sentinel on async handles with
/// `STATUS_INVALID_PARAMETER`.
const FILE_USE_FILE_POINTER_POSITION: i64 = -2;

/// Returns `true` iff `create_options` lacks both sync-IO flags. Async
/// handles enforce stricter `NtReadFile` preconditions (non-NULL
/// `ByteOffset`, sentinel rejection) regardless of any IOCP binding.
fn is_async_handle(create_options: ULONG) -> bool {
    create_options & (FILE_SYNCHRONOUS_IO_ALERT | FILE_SYNCHRONOUS_IO_NONALERT) == 0
}

/// Strategy for a single `NtReadFile` call on a managed file. Computed
/// up-front by [`classify_read`] so the hook body is a flat match
/// instead of a nested if-tree; each variant maps to one focused
/// helper that knows how to execute it.
enum ReadStrategy {
    /// `length == 0`: NT short-circuits without consulting the file.
    /// If the file is IOCP-bound, a zero-byte packet is still queued
    /// (the operation counts as an IO).
    ZeroLength,
    /// Async open-mode violation. Caller passed an invalid `ByteOffset`
    /// on a handle opened without `FILE_SYNCHRONOUS_IO_*`. The string
    /// is a static fragment naming which precondition tripped; it's
    /// surfaced in the warn log.
    InvalidAsync(&'static str),
    /// IOCP-bound + `skip_on_success`. Do the read inline; on success,
    /// skip the packet entirely (the high-throughput optimization
    /// clients like .NET enable).
    AsyncSkipOnSuccess {
        port: HANDLE,
        key: usize,
        offset: u64,
    },
    /// IOCP-bound, normal. Defer to a worker thread and return
    /// `STATUS_PENDING` immediately; the worker writes the IOSB +
    /// buffer and posts the completion packet.
    AsyncDeferred {
        port: HANDLE,
        key: usize,
        offset: u64,
    },
    /// Sync handle, NULL `ByteOffset`. Agent cursor IS the stored
    /// position; the read uses & advances it naturally.
    SyncCurrent,
    /// Sync handle, explicit `ByteOffset`. Save the cursor, seek to
    /// the offset, read, restore the cursor -- the NT contract is
    /// "explicit offset doesn't disturb the stored position."
    SyncExplicit { offset: u64 },
}

/// Decide which [`ReadStrategy`] applies. Pure function over the hook
/// inputs + handle context + IOCP binding state; no IO is issued
/// here.
///
/// SAFETY: caller must ensure `byte_offset` is null OR points at a
/// readable `LARGE_INTEGER` (the hook validates this before reaching
/// classification via async-handle pointer pre-flight, and the
/// classifier itself only dereferences when non-null).
unsafe fn classify_read(
    file: HANDLE,
    handle_context: &HandleContext,
    length: ULONG,
    byte_offset: PLARGE_INTEGER,
) -> ReadStrategy {
    if length == 0 {
        return ReadStrategy::ZeroLength;
    }

    let is_async = is_async_handle(handle_context.create_options);
    let binding = iocp::binding_for_file(file);

    // Async open-mode validation. Independent of binding so a
    // pre-bind read on a sync-flagged handle still fails the same way.
    if is_async {
        if byte_offset.is_null() {
            return ReadStrategy::InvalidAsync("requires non-NULL ByteOffset");
        }
        // SAFETY: non-null checked just above.
        if unsafe { *(*byte_offset).QuadPart() } == FILE_USE_FILE_POINTER_POSITION {
            return ReadStrategy::InvalidAsync("rejects FILE_USE_FILE_POINTER_POSITION sentinel");
        }
    }

    // IOCP-bound path. The kernel refuses `FileCompletionInformation`
    // bind on a sync file, so a port-bound file here is async in
    // practice; but the layer doesn't itself enforce that bind
    // precondition, so guard explicitly that we never deref a NULL
    // `byte_offset` here.
    if let Some((port, key)) = binding {
        if byte_offset.is_null() {
            return ReadStrategy::InvalidAsync("async-bound path requires non-NULL ByteOffset");
        }
        // SAFETY: non-null checked just above.
        let offset = unsafe { *(*byte_offset).QuadPart() } as u64;
        return if handle_context.skip_on_success {
            ReadStrategy::AsyncSkipOnSuccess { port, key, offset }
        } else {
            ReadStrategy::AsyncDeferred { port, key, offset }
        };
    }

    // Sync/inline path.
    if byte_offset.is_null() {
        ReadStrategy::SyncCurrent
    } else {
        // SAFETY: non-null checked just above.
        let offset = unsafe { *(*byte_offset).QuadPart() } as u64;
        ReadStrategy::SyncExplicit { offset }
    }
}

/// Body of `nt_read_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn handle(
    file: HANDLE,
    event: HANDLE,
    apc_routine: *mut c_void,
    apc_context: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
    byte_offset: PLARGE_INTEGER,
    key: PULONG,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(mut handle_context) = managed_handle.clone().try_write()
        {
            if let Err(status) = check_io_pointers(buffer, io_status_block) {
                tracing::warn!(
                    handle = ?file,
                    fd = handle_context.fd,
                    path = handle_context.path,
                    status_code = format!("{status:#x}"),
                    "nt_read_file_hook: invalid IO pointer pre-flight, returning status"
                );
                return status;
            }

            // Update last access time to current time, as we are reading.
            handle_context.access_time = WindowsTime::current().as_file_time();

            let strategy = classify_read(file, &handle_context, length, byte_offset);
            match strategy {
                ReadStrategy::ZeroLength => {
                    return do_zero_length(io_status_block, file, apc_context);
                }
                ReadStrategy::InvalidAsync(reason) => {
                    tracing::warn!(
                        path = handle_context.path,
                        fd = handle_context.fd,
                        reason,
                        "nt_read_file_hook: async handle precondition failed"
                    );
                    return STATUS_INVALID_PARAMETER;
                }
                ReadStrategy::AsyncSkipOnSuccess { port, key, offset } => {
                    return do_async_skip_on_success(
                        &handle_context,
                        port,
                        key,
                        offset,
                        length,
                        buffer,
                        io_status_block,
                        apc_context,
                    );
                }
                ReadStrategy::AsyncDeferred { port, key, offset } => {
                    return do_async_deferred(
                        handle_context,
                        handles,
                        port,
                        key,
                        offset,
                        length,
                        buffer,
                        io_status_block,
                        apc_context,
                    );
                }
                ReadStrategy::SyncCurrent => {
                    return do_sync_current(&handle_context, length, buffer, io_status_block);
                }
                ReadStrategy::SyncExplicit { offset } => {
                    return do_sync_explicit(
                        &handle_context,
                        offset,
                        length,
                        buffer,
                        io_status_block,
                    );
                }
            }
        }

        let original = NT_READ_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            buffer,
            length,
            byte_offset,
            key,
        )
    }
}

/// Implements [`ReadStrategy::ZeroLength`]. Stamp success IOSB, queue
/// a zero-byte completion packet if the file is bound to a port.
///
/// SAFETY: caller validated `io_status_block` via
/// [`check_io_pointers`].
unsafe fn do_zero_length(
    io_status_block: *mut _IO_STATUS_BLOCK,
    file: HANDLE,
    apc_context: PVOID,
) -> NTSTATUS {
    unsafe {
        write_iosb_success(io_status_block, 0);
        if let Some((port, completion_key)) = iocp::binding_for_file(file) {
            tracing::debug!(
                handle = ?file,
                ?port,
                key = completion_key,
                "nt_read_file_hook: length == 0 short-circuit, queueing zero-byte packet"
            );
            iocp::enqueue_packet(
                port,
                iocp::IocpPacket {
                    key: completion_key,
                    apc_context: apc_context as usize,
                    status: STATUS_SUCCESS,
                    information: 0,
                },
            );
        } else {
            tracing::debug!(
                handle = ?file,
                "nt_read_file_hook: length == 0 short-circuit (no port binding)"
            );
        }
        STATUS_SUCCESS
    }
}

/// Implements [`ReadStrategy::AsyncSkipOnSuccess`]. Do the read on
/// the caller's thread; if it succeeds, skip the packet entirely
/// (`FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`). On any non-success
/// status, queue a packet and return that status.
///
/// SAFETY: caller validated `buffer` + `io_status_block` via
/// [`check_io_pointers`].
#[allow(clippy::too_many_arguments)]
unsafe fn do_async_skip_on_success(
    handle_context: &HandleContext,
    port: HANDLE,
    completion_key: usize,
    offset: u64,
    length: ULONG,
    buffer: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    apc_context: PVOID,
) -> NTSTATUS {
    unsafe {
        tracing::debug!(
            path = handle_context.path,
            fd = handle_context.fd,
            ?port,
            key = completion_key,
            length,
            offset,
            "nt_read_file_hook: async (port-bound) skip-on-success inline read"
        );
        let (status, info) = perform_async_read(
            handle_context.fd,
            &handle_context.path,
            length,
            offset,
            buffer as usize,
        );
        write_iosb(io_status_block, status, info);
        if status == STATUS_SUCCESS {
            return STATUS_SUCCESS;
        }
        // Error path: queue packet so the dequeue side sees the failure.
        iocp::enqueue_packet(
            port,
            iocp::IocpPacket {
                key: completion_key,
                apc_context: apc_context as usize,
                status,
                information: info,
            },
        );
        status
    }
}

/// Implements [`ReadStrategy::AsyncDeferred`]. Capture everything the
/// worker needs into Send-safe primitives, drop the registry locks,
/// submit, return [`STATUS_PENDING`].
///
/// SAFETY: caller validated `buffer` + `io_status_block` via
/// [`check_io_pointers`]; both pointer lifetimes are owned by the NT
/// caller for the duration of the async IO per the NT contract.
#[allow(clippy::too_many_arguments)]
unsafe fn do_async_deferred(
    handle_context: std::sync::RwLockWriteGuard<'_, HandleContext>,
    handles: std::sync::RwLockReadGuard<
        '_,
        std::collections::HashMap<
            crate::hooks::files::managed_handle::MirrordFileHandle,
            std::sync::Arc<std::sync::RwLock<HandleContext>>,
        >,
    >,
    port: HANDLE,
    completion_key: usize,
    offset: u64,
    length: ULONG,
    buffer: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    apc_context: PVOID,
) -> NTSTATUS {
    let fd = handle_context.fd;
    let path = handle_context.path.clone();
    let buffer_addr = buffer as usize;
    let iosb_addr = io_status_block as usize;
    let port_addr = port as usize;
    let apc_addr = apc_context as usize;

    tracing::debug!(
        path,
        fd,
        ?port,
        key = completion_key,
        length,
        offset,
        "nt_read_file_hook: async (port-bound) deferred to worker, returning STATUS_PENDING"
    );

    drop(handle_context);
    drop(handles);

    iocp::worker::submit(move || {
        do_async_read(
            fd,
            path,
            length,
            offset,
            buffer_addr,
            iosb_addr,
            port_addr,
            completion_key,
            apc_addr,
        );
    });
    STATUS_PENDING
}

/// Implements [`ReadStrategy::SyncCurrent`]. NULL `ByteOffset` on a
/// sync handle: the agent cursor IS the stored position; the read
/// advances it naturally.
///
/// SAFETY: caller validated `buffer` + `io_status_block` via
/// [`check_io_pointers`].
unsafe fn do_sync_current(
    handle_context: &HandleContext,
    length: ULONG,
    buffer: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
) -> NTSTATUS {
    unsafe {
        tracing::debug!(
            path = handle_context.path,
            fd = handle_context.fd,
            length,
            "nt_read_file_hook: sync (inline) read, current cursor"
        );
        let bytes = match make_proxy_request_with_response(ReadFileRequest {
            remote_fd: handle_context.fd,
            buffer_size: length as _,
        }) {
            Ok(Ok(res)) => res.bytes,
            _ => {
                tracing::error!(
                    fd = handle_context.fd,
                    path = handle_context.path,
                    "nt_read_file_hook: Pod did not return a buffer when reading file!"
                );
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            }
        };
        finish_sync_read(buffer, io_status_block, length, &bytes)
    }
}

/// Implements [`ReadStrategy::SyncExplicit`]. Save the cursor, seek
/// to `offset`, read, restore the cursor. NT contract: explicit
/// offset on sync handles doesn't disturb the stored position.
///
/// SAFETY: caller validated `buffer` + `io_status_block` via
/// [`check_io_pointers`].
unsafe fn do_sync_explicit(
    handle_context: &HandleContext,
    offset: u64,
    length: ULONG,
    buffer: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
) -> NTSTATUS {
    unsafe {
        tracing::debug!(
            path = handle_context.path,
            fd = handle_context.fd,
            length,
            offset,
            "nt_read_file_hook: sync (inline) read, explicit offset (save/seek/read/restore)"
        );
        let saved = match try_seek(handle_context.fd, SeekFromInternal::Current(0)) {
            Some(v) => v,
            None => {
                tracing::error!(
                    fd = handle_context.fd,
                    path = handle_context.path,
                    "nt_read_file_hook: Failed peeking cursor before explicit-offset read"
                );
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            }
        };
        if try_seek(handle_context.fd, SeekFromInternal::Start(offset)).is_none() {
            tracing::error!(
                fd = handle_context.fd,
                path = handle_context.path,
                offset,
                "nt_read_file_hook: Failed seeking to explicit offset"
            );
            return STATUS_UNEXPECTED_NETWORK_ERROR;
        }

        let bytes = match make_proxy_request_with_response(ReadFileRequest {
            remote_fd: handle_context.fd,
            buffer_size: length as _,
        }) {
            Ok(Ok(res)) => res.bytes,
            _ => {
                let _ = try_seek(handle_context.fd, SeekFromInternal::Start(saved as _));
                tracing::error!(
                    fd = handle_context.fd,
                    path = handle_context.path,
                    "nt_read_file_hook: Pod did not return a buffer when reading file!"
                );
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            }
        };

        if try_seek(handle_context.fd, SeekFromInternal::Start(saved as _)).is_none() {
            tracing::error!(
                fd = handle_context.fd,
                path = handle_context.path,
                "nt_read_file_hook: Failed restoring cursor after explicit-offset read"
            );
            return STATUS_UNEXPECTED_NETWORK_ERROR;
        }

        finish_sync_read(buffer, io_status_block, length, &bytes)
    }
}

/// Shared tail of the sync read paths: empty agent response →
/// `STATUS_END_OF_FILE`; otherwise copy + stamp IOSB +
/// `STATUS_SUCCESS`.
unsafe fn finish_sync_read(
    buffer: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    length: ULONG,
    bytes: &[u8],
) -> NTSTATUS {
    unsafe {
        if bytes.is_empty() {
            write_iosb_eof(io_status_block);
            return STATUS_END_OF_FILE;
        }
        let len = usize::min(bytes.len(), length as _);
        std::ptr::copy(bytes.as_ptr(), buffer as _, len);
        write_iosb_success(io_status_block, len);
        STATUS_SUCCESS
    }
}

/// Worker routine for the async path. Runs on a thread from
/// [`iocp::worker`] so the caller of `NtReadFile` can return
/// `STATUS_PENDING` immediately.
///
/// Performs save/seek/read/restore against the agent, writes the
/// user-provided buffer + IOSB, then posts a completion packet to
/// the bound port. `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` is handled
/// inline in [`handle`] above before submit, so the worker is always
/// responsible for posting a packet, the user is waiting.
#[allow(clippy::too_many_arguments)]
fn do_async_read(
    fd: u64,
    path: String,
    length: ULONG,
    offset: u64,
    buffer_addr: usize,
    iosb_addr: usize,
    port_addr: usize,
    key: usize,
    apc_context: usize,
) {
    let (status, info) = perform_async_read(fd, &path, length, offset, buffer_addr);

    // SAFETY: caller owns the IOSB pointer's lifetime per NT contract.
    unsafe { write_iosb(iosb_addr as *mut _IO_STATUS_BLOCK, status, info) };

    iocp::enqueue_packet(
        port_addr as HANDLE,
        iocp::IocpPacket {
            key,
            apc_context,
            status,
            information: info,
        },
    );
}

/// Agent round-trip for an async read with save/seek/read/restore so
/// the stored file position survives untouched (NT contract for
/// explicit-offset reads).
fn perform_async_read(
    fd: u64,
    path: &str,
    length: ULONG,
    offset: u64,
    buffer_addr: usize,
) -> (NTSTATUS, usize) {
    let saved = match try_seek(fd, SeekFromInternal::Current(0)) {
        Some(v) => v,
        None => {
            tracing::error!(fd, path, "perform_async_read: failed peeking cursor");
            return (STATUS_UNEXPECTED_NETWORK_ERROR, 0);
        }
    };
    if try_seek(fd, SeekFromInternal::Start(offset)).is_none() {
        tracing::error!(
            fd,
            path,
            offset,
            "perform_async_read: failed seeking to offset"
        );
        return (STATUS_UNEXPECTED_NETWORK_ERROR, 0);
    }

    let bytes = match make_proxy_request_with_response(ReadFileRequest {
        remote_fd: fd,
        buffer_size: length as _,
    }) {
        Ok(Ok(res)) => res.bytes,
        _ => {
            let _ = try_seek(fd, SeekFromInternal::Start(saved as _));
            tracing::error!(fd, path, "perform_async_read: ReadFileRequest failed");
            return (STATUS_UNEXPECTED_NETWORK_ERROR, 0);
        }
    };

    if try_seek(fd, SeekFromInternal::Start(saved as _)).is_none() {
        tracing::error!(fd, path, "perform_async_read: failed restoring cursor");
    }

    if bytes.is_empty() {
        return (STATUS_END_OF_FILE, 0);
    }
    let len = usize::min(bytes.len(), length as usize);
    // SAFETY: caller owns the buffer's lifetime per NT contract.
    unsafe {
        std::ptr::copy(bytes.as_ptr(), buffer_addr as *mut u8, len);
    }
    (STATUS_SUCCESS, len)
}

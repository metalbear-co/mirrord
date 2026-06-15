//! Bodies of the unimplemented file hooks. The hooks in the
//! [FS-hook dispatcher](super::super) are thin delegates into each of
//! these.
//!
//! Every stub here is either a pure passthrough or a "warn-if-managed,
//! then fall through to the original NT syscall" body. The warn is
//! load-bearing: it's the only signal we get in production when a
//! managed-file caller hits a hook we don't yet remote, and so drives
//! the prioritization of what to implement next. Don't refactor the
//! warns away when the body looks "obviously repetitive" -- keep them
//! hand-written so each one can grow independently (an extra `?arg`
//! field in the trace, a partial implementation, ad-hoc instrumentation
//! during a debug session, etc.).
//!
//! Stubs covered here:
//!
//! - [`write`](fn@write): write IO against managed files is not yet remoted; pure passthrough.
//! - [`set_volume_information`]: volume-level metadata writes.
//! - [`set_quota_information`]: per-user quota writes.
//! - [`query_attributes`]: path-based attribute query (no handle, so the warn uses
//!   [`for_each_handle_with_path`] to enumerate any managed handles on the same path).
//! - [`query_quota_information`]: per-user quota queries.
//! - [`delete_file`]: write IO is out of scope; path-based warn via [`for_each_handle_with_path`].
//! - [`device_io_control`]: arbitrary IOCTL; the input buffer is untyped/unsafe to print, so we
//!   don't trace inside the if-managed block.
//! - [`lock_file`] / [`unlock_file`]: range locking on managed files isn't remoted.

use std::ffi::c_void;

use phnt::ffi::{_IO_STATUS_BLOCK, FSINFOCLASS, PFILE_BASIC_INFORMATION, PIO_APC_ROUTINE};
use winapi::{
    shared::ntdef::{
        BOOLEAN, HANDLE, NTSTATUS, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID, ULONG,
    },
    um::winnt::PSID,
};

use crate::hooks::files::{
    managed_handle::{MANAGED_HANDLES, for_each_handle_with_path},
    types::{
        NT_DELETE_FILE_ORIGINAL, NT_DEVICE_IO_CONTROL_FILE_ORIGINAL, NT_LOCK_FILE_ORIGINAL,
        NT_QUERY_ATTRIBUTES_FILE_ORIGINAL, NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL,
        NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL, NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL,
        NT_UNLOCK_FILE_ORIGINAL, NT_WRITE_FILE_ORIGINAL,
    },
};

/// Body of `nt_write_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn write(
    file: HANDLE,
    event: HANDLE,
    apc_routine: *mut c_void,
    apc_context: PVOID,
    io_status_block: *mut c_void,
    buffer: PVOID,
    length: ULONG,
    byte_offset: PLARGE_INTEGER,
    key: PULONG,
) -> NTSTATUS {
    unsafe {
        let original = NT_WRITE_FILE_ORIGINAL.get().unwrap();
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

/// Body of `nt_set_volume_information_file_hook`.
pub(in crate::hooks::files) unsafe fn set_volume_information(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    fs_info_class: FSINFOCLASS,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            tracing::warn!(
                path = handle_context.path,
                ?fs_info_class,
                "nt_set_volume_information_file_hook: Not implemented! Failling back on original!"
            );
        }

        let original = NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            fs_info_class,
        )
    }
}

/// Body of `nt_set_quota_information_file_hook`.
pub(in crate::hooks::files) unsafe fn set_quota_information(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            tracing::warn!(
                path = handle_context.path,
                "nt_set_quota_information_file_hook: Not implemented! Failling back on original!"
            );
        }

        let original = NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(file, io_status_block, buffer, length)
    }
}

/// Body of `nt_query_attributes_file_hook`.
///
/// The syscall takes a `POBJECT_ATTRIBUTES` (no handle), so we use
/// [`for_each_handle_with_path`] to warn for any managed handle that
/// shares the path.
pub(in crate::hooks::files) unsafe fn query_attributes(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe {
        for_each_handle_with_path(object_attributes, |handle, handle_context| {
            tracing::warn!(
                path = handle_context.path,
                "nt_query_attributes_file_hook: Function not implemented! (handle: {:08x})",
                handle.raw() as usize
            );
        });

        let original = NT_QUERY_ATTRIBUTES_FILE_ORIGINAL.get().unwrap();
        original(object_attributes, file_basic_info)
    }
}

/// Body of `nt_query_quota_information_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn query_quota_information(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
    return_single_entry: BOOLEAN,
    sid_list: PVOID,
    sid_list_length: ULONG,
    start_sid: PSID,
    restart_scan: BOOLEAN,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            tracing::warn!(
                path = handle_context.path,
                "nt_query_quota_information_file_hook: Not implemented! Failling back on original!"
            );
        }

        let original = NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            buffer,
            length,
            return_single_entry,
            sid_list,
            sid_list_length,
            start_sid,
            restart_scan,
        )
    }
}

/// Body of `nt_delete_file_hook`. Path-based, so the warn enumerates
/// managed handles on the same path via [`for_each_handle_with_path`].
pub(in crate::hooks::files) unsafe fn delete_file(
    object_attributes: POBJECT_ATTRIBUTES,
) -> NTSTATUS {
    unsafe {
        for_each_handle_with_path(object_attributes, |handle, handle_context| {
            tracing::warn!(
                path = handle_context.path,
                "nt_delete_file_hook: Attempt to delete file that current handle ({:8x}) points to! Not implemented! Will fall back on original!",
                handle.raw() as usize
            );
        });

        let original = NT_DELETE_FILE_ORIGINAL.get().unwrap();
        original(object_attributes)
    }
}

/// Body of `nt_device_io_control_file_hook`.
///
/// NOTE(gabriela): SUPER unsafe to print in! The input buffer is
/// untyped (caller-defined per IOCTL) and may be huge or sensitive,
/// so we keep the if-managed block empty. It exists as a single place
/// to add a targeted log during an ad-hoc debug session.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn device_io_control(
    file: HANDLE,
    event: HANDLE,
    apc_routine: PIO_APC_ROUTINE,
    apc_context: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    io_control_code: ULONG,
    input_buffer: PVOID,
    input_buffer_length: ULONG,
    output_buffer: PVOID,
    output_buffer_length: ULONG,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(_handle_context) = managed_handle.try_read()
        {
            // unsafe to print in
        }

        let original = NT_DEVICE_IO_CONTROL_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            io_control_code,
            input_buffer,
            input_buffer_length,
            output_buffer,
            output_buffer_length,
        )
    }
}

/// Body of `nt_lock_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn lock_file(
    file: HANDLE,
    event: HANDLE,
    apc_routine: PIO_APC_ROUTINE,
    apc_context: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    byte_offset: PLARGE_INTEGER,
    length: PLARGE_INTEGER,
    key: ULONG,
    fail_immediately: BOOLEAN,
    exclusive_lock: BOOLEAN,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            tracing::warn!(
                path = handle_context.path,
                "nt_lock_file_hook: Not implemented! Failling back on original!",
            );
        }

        let original = NT_LOCK_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            byte_offset,
            length,
            key,
            fail_immediately,
            exclusive_lock,
        )
    }
}

/// Body of `nt_unlock_file_hook`. See [`lock_file`].
pub(in crate::hooks::files) unsafe fn unlock_file(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    byte_offset: PLARGE_INTEGER,
    length: PLARGE_INTEGER,
    key: ULONG,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            tracing::warn!(
                path = handle_context.path,
                "nt_unlock_file_hook: Not implemented! Failling back on original!",
            );
        }

        let original = NT_UNLOCK_FILE_ORIGINAL.get().unwrap();
        original(file, io_status_block, byte_offset, length, key)
    }
}

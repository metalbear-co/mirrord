//! Module responsible for registering hooks targetting file operations syscalls.
//!
//! ## Layout
//!
//! - [`types`] -- NT function-pointer aliases + `OnceLock`s for the originals (resolved at
//!   hook-install time and reused on every fallthrough).
//! - [`iosb`] -- pointer pre-flight + `IO_STATUS_BLOCK` writers (hides the bindgen
//!   `ManuallyDrop<union>` ceremony).
//! - [`managed_handle`] -- the `MirrordFileHandle` registry and per- handle
//!   [`HandleContext`](managed_handle::HandleContext).
//! - [`util`] -- shared helpers (`WindowsTime`, NT-path classifier, agent `try_seek` /
//!   `try_xstat`).
//! - [`ops`] -- one module per FS operation. Each contains the body that used to live inline in
//!   this file; the `unsafe extern "system"` hooks below are thin delegates so all that lives here
//!   is the dispatch table + initialization.
//!
//! ## NT level
//!
//! NOTE(gabriela): this is SUPER unsafe to instrument, as printing in the
//! context of hooks for writing to files/handles (includes stdout/stderr
//! in scope) naturally overlaps with the functions that would be called
//! while tracing. As a result, there's no instrumenting over the hooks!
//!
//! This is also a problem on the Unix end:
//! <https://github.com/metalbear-co/mirrord/issues/425>
//!
//! In general, you must be careful about your printing in this scope. We
//! should rethink this approach to logging completely, if I'm honest. But
//! how?

use std::ffi::c_void;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::LayerResult;
use phnt::ffi::{
    _IO_STATUS_BLOCK, FILE_INFORMATION_CLASS, FSINFOCLASS, PFILE_BASIC_INFORMATION, PIO_APC_ROUTINE,
};
use winapi::{
    shared::ntdef::{
        BOOLEAN, HANDLE, NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID,
        ULONG,
    },
    um::winnt::{ACCESS_MASK, PSID},
};

use crate::apply_hook;

mod iosb;
mod managed_handle;
mod ops;
mod types;
mod util;

use self::types::*;

// ---------------------------------------------------------------------------
// Hook delegates. Each `unsafe extern "system" fn` here is a thin
// shim over the matching `ops::<op>::handle` (or named function in
// `ops::query_info` / `ops::stubs`). The function bodies live in
// `ops/` so this file stays a dispatch table.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn nt_create_file_hook(
    file_handle: PHANDLE,
    desired_access: ACCESS_MASK,
    object_attributes: POBJECT_ATTRIBUTES,
    io_status_block: *mut _IO_STATUS_BLOCK,
    allocation_size: PLARGE_INTEGER,
    file_attributes: ULONG,
    share_access: ULONG,
    create_disposition: ULONG,
    create_options: ULONG,
    ea_buf: PVOID,
    ea_size: ULONG,
) -> NTSTATUS {
    unsafe {
        ops::open::handle(
            file_handle,
            desired_access,
            object_attributes,
            io_status_block,
            allocation_size,
            file_attributes,
            share_access,
            create_disposition,
            create_options,
            ea_buf,
            ea_size,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn nt_read_file_hook(
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
        ops::read::handle(
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

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn nt_write_file_hook(
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
        ops::stubs::write(
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

unsafe extern "system" fn nt_set_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        ops::set_info::handle(
            file,
            io_status_block,
            file_information,
            length,
            file_information_class,
        )
    }
}

unsafe extern "system" fn nt_set_volume_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    fs_info_class: FSINFOCLASS,
) -> NTSTATUS {
    unsafe {
        ops::stubs::set_volume_information(
            file,
            io_status_block,
            file_information,
            length,
            fs_info_class,
        )
    }
}

unsafe extern "system" fn nt_set_quota_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
) -> NTSTATUS {
    unsafe { ops::stubs::set_quota_information(file, io_status_block, buffer, length) }
}

unsafe extern "system" fn nt_query_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        ops::query_info::info(
            file,
            io_status_block,
            file_information,
            length,
            file_information_class,
        )
    }
}

unsafe extern "system" fn nt_query_attributes_file_hook(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe { ops::stubs::query_attributes(object_attributes, file_basic_info) }
}

unsafe extern "system" fn nt_query_volume_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    fs_info_class: FSINFOCLASS,
) -> NTSTATUS {
    unsafe {
        ops::query_info::volume_info(
            file,
            io_status_block,
            file_information,
            length,
            fs_info_class,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn nt_query_quota_information_file_hook(
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
        ops::stubs::query_quota_information(
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

unsafe extern "system" fn nt_delete_file_hook(object_attributes: POBJECT_ATTRIBUTES) -> NTSTATUS {
    unsafe { ops::stubs::delete_file(object_attributes) }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn nt_device_io_control_file_hook(
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
        ops::stubs::device_io_control(
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

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn nt_lock_file_hook(
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
        ops::stubs::lock_file(
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

unsafe extern "system" fn nt_unlock_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    byte_offset: PLARGE_INTEGER,
    length: PLARGE_INTEGER,
    key: ULONG,
) -> NTSTATUS {
    unsafe { ops::stubs::unlock_file(file, io_status_block, byte_offset, length, key) }
}

unsafe extern "system" fn nt_close_hook(handle: HANDLE) -> NTSTATUS {
    unsafe { ops::close::handle(handle) }
}

unsafe extern "system" fn nt_cancel_io_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
) -> NTSTATUS {
    unsafe { ops::cancel::handle(file, io_status_block) }
}

unsafe extern "system" fn nt_wait_for_single_object_hook(
    handle: HANDLE,
    alertable: BOOLEAN,
    timeout: PLARGE_INTEGER,
) -> NTSTATUS {
    unsafe { ops::wait::handle(handle, alertable, timeout) }
}

pub(super) fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
    // ----------------------------------------------------------------------------
    // ~NOTE(gabriela):
    //
    // Staying at NT level is a necessity in the attempt to catch every single
    // user-mode level file access. That is as low-level as we can possibly go, and
    // it means that there are no leaks (as far as no driver code, or manually
    // crafted syscalls are involved).
    //
    // It also means that, if we handle logic at the lowest level correctly,
    // the functionality will propagate to every single possible path of opening,
    // reading, etc. a file. Which means, we will inherently support every single
    // runtime (Rust, C#, Python, Node, ...) without any special logic involved
    // (unless it somehow proves itself to be required.)
    //
    // We have already thoroughly tested this with Python and POSIX APIs.
    //
    // Every single Win32 function that interacts with files, in one way or another
    // defers itself to a NT function, making implementations at kernelbase level
    // inherrently a risk of abstraction leaks, which is a bug. As a result, we
    // treat that approach as simply incorrect.
    //
    // Implementations of NT level hooks should have behavior identical to the real
    // NT function calls, and in turn automatically support it when kernelbase
    // wraps around it, and depends on specific returns, etc.
    // ----------------------------------------------------------------------------

    // Core file operation hooks
    apply_hook!(
        guard,
        "ntdll",
        "NtCreateFile",
        nt_create_file_hook,
        NtCreateFileType,
        NT_CREATE_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtClose",
        nt_close_hook,
        NtCloseType,
        NT_CLOSE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtCancelIoFile",
        nt_cancel_io_file_hook,
        NtCancelIoFileType,
        NT_CANCEL_IO_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtWaitForSingleObject",
        nt_wait_for_single_object_hook,
        NtWaitForSingleObjectType,
        NT_WAIT_FOR_SINGLE_OBJECT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtSetInformationFile",
        nt_set_information_file_hook,
        NtSetInformationFileType,
        NT_SET_INFORMATION_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtQueryInformationFile",
        nt_query_information_file_hook,
        NtQueryInformationFileType,
        NT_QUERY_INFORMATION_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtQueryVolumeInformationFile",
        nt_query_volume_information_file_hook,
        NtQueryVolumeInformationFileType,
        NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL
    )?;

    // Read hooks
    apply_hook!(
        guard,
        "ntdll",
        "NtReadFile",
        nt_read_file_hook,
        NtReadFileType,
        NT_READ_FILE_ORIGINAL
    )?;

    // Write hooks
    apply_hook!(
        guard,
        "ntdll",
        "NtWriteFile",
        nt_write_file_hook,
        NtWriteFileType,
        NT_WRITE_FILE_ORIGINAL
    )?;

    // Metadata and directory hooks

    // ----------------------------------------------------------------------------
    // NOTE(gabriela): the following hooks are unimplemented!
    // They're only here to trace missing logic! Please move above once they're
    // implemented!

    apply_hook!(
        guard,
        "ntdll",
        "NtSetVolumeInformationFile",
        nt_set_volume_information_file_hook,
        NtSetVolumeInformationFileType,
        NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtSetQuotaInformationFile",
        nt_set_quota_information_file_hook,
        NtSetQuotaInformationFileType,
        NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtQueryAttributesFile",
        nt_query_attributes_file_hook,
        NtQueryAttributesFileType,
        NT_QUERY_ATTRIBUTES_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtQueryQuotaInformationFile",
        nt_query_quota_information_file_hook,
        NtQueryQuotaInformationFileType,
        NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtDeleteFile",
        nt_delete_file_hook,
        NtDeleteFileType,
        NT_DELETE_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtDeviceIoControlFile",
        nt_device_io_control_file_hook,
        NtDeviceIoControlFileType,
        NT_DEVICE_IO_CONTROL_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtLockFile",
        nt_lock_file_hook,
        NtLockFileType,
        NT_LOCK_FILE_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtUnlockFile",
        nt_unlock_file_hook,
        NtUnlockFileType,
        NT_UNLOCK_FILE_ORIGINAL
    )?;

    tracing::info!("File hooks initialization completed");
    Ok(())
}

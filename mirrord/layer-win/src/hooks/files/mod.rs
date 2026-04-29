//! Module responsible for registering hooks targetting file operations syscalls.
//!
//! NOTE(gabriela): this is SUPER unsafe to instrument, as printing in the context
//! of hooks for writing to files/handles (includes stdout/stderr in scope) naturally
//! overlaps with the functions that would be called while tracing. As a result, there's no
//! instrumenting over the hooks!
//!
//! This is also a problem on the Unix end: https://github.com/metalbear-co/mirrord/issues/425
//!
//! In general, you must be careful about your printing in this scope. We should rethink
//! this approach to logging completely, if I'm honest. But how?

use std::{ffi::c_void, mem::ManuallyDrop, path::PathBuf, sync::OnceLock};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    LayerResult,
    file::filter::FileMode,
    proxy_connection::{make_proxy_request_no_response, make_proxy_request_with_response},
    setup::setup,
};
use mirrord_protocol::file::{
    CloseFileRequest, OpenFileRequest, OpenOptionsInternal, ReadFileRequest, SeekFromInternal,
};
use phnt::ffi::{
    _IO_STATUS_BLOCK, _LARGE_INTEGER, FILE_ALL_INFORMATION, FILE_BASIC_INFORMATION,
    FILE_FS_DEVICE_INFORMATION, FILE_FS_VOLUME_INFORMATION, FILE_INFORMATION_CLASS,
    FILE_POSITION_INFORMATION, FILE_READ_ONLY_DEVICE, FILE_STANDARD_INFORMATION, FSINFOCLASS,
    PFILE_BASIC_INFORMATION, PIO_APC_ROUTINE,
};
use str_win::path_to_unix_path;
use winapi::{
    shared::{
        minwindef::FILETIME,
        ntdef::{
            BOOLEAN, FALSE, HANDLE, NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG,
            PVOID, ULONG,
        },
        ntstatus::{
            STATUS_ACCESS_VIOLATION, STATUS_END_OF_FILE, STATUS_OBJECT_PATH_NOT_FOUND,
            STATUS_SUCCESS, STATUS_UNEXPECTED_NETWORK_ERROR,
        },
    },
    um::{
        winioctl::FILE_DEVICE_VIRTUAL_DISK,
        winnt::{
            ACCESS_MASK, FILE_APPEND_DATA, FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES,
            FILE_WRITE_DATA, GENERIC_WRITE, PSID,
        },
    },
};

use crate::{
    apply_hook,
    hooks::files::{
        managed_handle::{
            HandleContext, MANAGED_HANDLES, for_each_handle_with_path, try_insert_handle,
        },
        util::{
            WindowsTime, is_nt_path_disk_path, read_object_attributes_name, try_seek, try_xstat,
        },
    },
    process::memory::is_memory_valid,
};

pub mod managed_handle;
pub mod util;

// https://github.com/winsiderss/phnt/blob/fc1f96ee976635f51faa89896d1d805eb0586350/ntioapi.h#L1611
type NtCreateFileType = unsafe extern "system" fn(
    PHANDLE,
    ACCESS_MASK,
    POBJECT_ATTRIBUTES,
    *mut _IO_STATUS_BLOCK,
    PLARGE_INTEGER,
    ULONG,
    ULONG,
    ULONG,
    ULONG,
    PVOID,
    ULONG,
) -> NTSTATUS;
static NT_CREATE_FILE_ORIGINAL: OnceLock<&NtCreateFileType> = OnceLock::new();

// https://github.com/winsiderss/phnt/blob/fc1f96ee976635f51faa89896d1d805eb0586350/ntioapi.h#L1963
type NtReadFileType = unsafe extern "system" fn(
    HANDLE,
    HANDLE,
    *mut c_void,
    PVOID,
    *mut _IO_STATUS_BLOCK,
    PVOID,
    ULONG,
    PLARGE_INTEGER,
    PULONG,
) -> NTSTATUS;
static NT_READ_FILE_ORIGINAL: OnceLock<&NtReadFileType> = OnceLock::new();

// https://github.com/winsiderss/phnt/blob/fc1f96ee976635f51faa89896d1d805eb0586350/ntioapi.h#L1978
type NtWriteFileType = unsafe extern "system" fn(
    HANDLE,
    HANDLE,
    *mut c_void,
    PVOID,
    *mut c_void,
    PVOID,
    ULONG,
    PLARGE_INTEGER,
    PULONG,
) -> NTSTATUS;
static NT_WRITE_FILE_ORIGINAL: OnceLock<&NtWriteFileType> = OnceLock::new();

type NtSetInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PVOID,
    ULONG,
    FILE_INFORMATION_CLASS,
) -> NTSTATUS;
static NT_SET_INFORMATION_FILE_TYPE_ORIGINAL: OnceLock<&NtSetInformationFileType> = OnceLock::new();

type NtSetVolumeInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK, PVOID, ULONG, FSINFOCLASS) -> NTSTATUS;
static NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL: OnceLock<&NtSetVolumeInformationFileType> =
    OnceLock::new();

type NtSetQuotaInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK, PVOID, ULONG) -> NTSTATUS;
static NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL: OnceLock<&NtSetQuotaInformationFileType> =
    OnceLock::new();

type NtQueryInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PVOID,
    ULONG,
    FILE_INFORMATION_CLASS,
) -> NTSTATUS;
static NT_QUERY_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryInformationFileType> = OnceLock::new();

type NtQueryAttributesFileType =
    unsafe extern "system" fn(POBJECT_ATTRIBUTES, PFILE_BASIC_INFORMATION) -> NTSTATUS;
static NT_QUERY_ATTRIBUTES_FILE_ORIGINAL: OnceLock<&NtQueryAttributesFileType> = OnceLock::new();

type NtQueryVolumeInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK, PVOID, ULONG, FSINFOCLASS) -> NTSTATUS;
static NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryVolumeInformationFileType> =
    OnceLock::new();

type NtQueryQuotaInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PVOID,
    ULONG,
    BOOLEAN,
    PVOID,
    ULONG,
    PSID,
    BOOLEAN,
) -> NTSTATUS;
static NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryQuotaInformationFileType> =
    OnceLock::new();

type NtDeleteFileType = unsafe extern "system" fn(POBJECT_ATTRIBUTES) -> NTSTATUS;
static NT_DELETE_FILE_ORIGINAL: OnceLock<&NtDeleteFileType> = OnceLock::new();

type NtDeviceIoControlFileType = unsafe extern "system" fn(
    HANDLE,
    HANDLE,
    PIO_APC_ROUTINE,
    PVOID,
    *mut _IO_STATUS_BLOCK,
    ULONG,
    PVOID,
    ULONG,
    PVOID,
    ULONG,
) -> NTSTATUS;
static NT_DEVICE_IO_CONTROL_FILE_ORIGINAL: OnceLock<&NtDeviceIoControlFileType> = OnceLock::new();

type NtLockFileType = unsafe extern "system" fn(
    HANDLE,
    HANDLE,
    PIO_APC_ROUTINE,
    PVOID,
    *mut _IO_STATUS_BLOCK,
    PLARGE_INTEGER,
    PLARGE_INTEGER,
    ULONG,
    BOOLEAN,
    BOOLEAN,
) -> NTSTATUS;
static NT_LOCK_FILE_ORIGINAL: OnceLock<&NtLockFileType> = OnceLock::new();

type NtUnlockFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PLARGE_INTEGER,
    PLARGE_INTEGER,
    ULONG,
) -> NTSTATUS;
static NT_UNLOCK_FILE_ORIGINAL: OnceLock<&NtUnlockFileType> = OnceLock::new();

type NtCloseType = unsafe extern "system" fn(HANDLE) -> NTSTATUS;
static NT_CLOSE_ORIGINAL: OnceLock<&NtCloseType> = OnceLock::new();

/// [`nt_create_file_hook`] is the function responsible for catching attempts to create a
/// new file handle. In turn, it is also responsible for catching the attempt to create
/// POSIX file descriptors through the Windows POSIX compatibility layer.
///
/// The mechanism is:
///
/// 1. We check if the layer is configured to have file-system hooks enbled or not.
///     * If not, jump to ["Fallback"](#fallback).
///     * This is the only point we have to check for this configuration. Without
///       [`managed_handle::MirrordHandle`]s, there is no other logic.
/// 2. We check if the path from `object_attributes` is an NT disk path.
///     * If we fail, jump to ["Fallback"](#fallback).
/// 3. We try to convert the path from `object_attributes` to a Unix path.
///     * If we fail, jump to ["Fallback"](#fallback).
/// 4. We run the Unix path through
///    [`mirrord_layer_lib::file::mapper::FileRemapper::change_path_str`] to account for file-system
///    configuration, This becomes the new "path".
///     * If the result of this operation is different from the original path, this is logged.
/// 5. We account for filter logic after we run the Unix path through
///    [`mirrord_layer_lib::file::filter::FileFilter::check`] to account for file-system
///    configuration.
/// 6. We check if all input arguments are valid (verify pointer provenance, value, etc.)
///     * If any is not valid, log and jump to ["Fallback"](#fallback).
/// 7. We make an [`mirrord_protocol::file::OpenFileRequest`] for the Unix path to obtain a file
///    descriptor from agent.
///     * If this fails, log and jump to ["Fallback"](#fallback).
/// 8. Use the file descriptor from the [`mirrord_protocol::file::OpenFileResponse`] to create a
///    [`HandleContext`] and insert it into the managed handles map.
///     * The [`HandleContext`] may be modified by future operations over the
///       [`managed_handle::MirrordHandle`].
///
/// ## Fallback
///
/// Discard previous progress, proceed with original execution by the operating system.
///
/// ## Caveats
///
/// - Currently, file-system write operations are not supported.
/// - Currently, directory operations are not supported.
///
/// ## Notes
///
/// mirrord handles can easily be identified by debugging because of the following:
/// - They are inserted starting from the numeric value `0x50000000`.
/// - They grow in linear numeric order from the starting point.
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
        // Closure to prevent repeating this long invocation ad-nauseam.
        let run_original = || -> NTSTATUS {
            let original = NT_CREATE_FILE_ORIGINAL.get().unwrap();
            original(
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
        };

        let setup = setup();
        // NOTE(gabriela): this is the only place, realistically, where this logic handling
        // is even needed in the first place, as the lack of [`MirrordHandle`]s will propagate
        // to all functions which rely on expecting one!
        if !setup.fs_hooks_enabled() {
            return run_original();
        }

        let name = read_object_attributes_name(object_attributes);

        // WIN-56: check if path is a FS path.
        if !is_nt_path_disk_path(&name) {
            return run_original();
        }

        // Try to create a Linux path from the provided Windows NT path.
        let Some(parsed_unix_path) = path_to_unix_path(name.clone()) else {
            return run_original();
        };

        // Some frameworks try to open the drive root (e.g. `C:\`) to inspect metadata, e.g. Node.
        // We don't support mirroring root directory handles, so let Windows handle these locally.
        if parsed_unix_path == "/" {
            tracing::debug!(
                "nt_create_file_hook: bypassing remote open for root directory \"{}\"",
                name
            );
            return run_original();
        }

        // NOTE(gabriela): (fs config) - we must make sure to follow the same routine
        // as Unix fs config handling does.

        // NOTE(gabriela): (fs config)[1] - take original path, run through mapper
        let mapper = setup.file_remapper();
        let unix_path = String::from(mapper.change_path_str(parsed_unix_path.as_str()));

        // NOTE(gabriela): print when a file mapping is found.
        if parsed_unix_path != unix_path {
            tracing::info!(
                "nt_create_file_hook: mapping matched, \"{}\" -> \"{}\"",
                parsed_unix_path,
                unix_path
            );
        }

        // NOTE(gabriela): not happy about the clone, research implications!
        let filter = setup.file_filter();

        // NOTE(gabriela): (fs config)[2] - take mapped path, apply filters
        // NOTE(gabriela): look up if there are any filter settings for the path.
        // see how it collides with desired_access.
        let matched_filter = filter.check(&unix_path);
        match matched_filter {
            Some(FileMode::Local(_)) => {
                tracing::trace!("nt_create_file_hook: reading \"{}\" locally!", name);
                return run_original();
            }
            Some(FileMode::NotFound(_)) => {
                *file_handle = std::ptr::null_mut();
                *io_status_block = _IO_STATUS_BLOCK::default();
                return STATUS_OBJECT_PATH_NOT_FOUND;
            }
            // TODO(gabriela): edit when supported!
            Some(FileMode::ReadOnly(_)) | Some(FileMode::ReadWrite(_)) | None => {
                // Check if the caller is attempting to get a write capable handle.
                if desired_access & FILE_WRITE_DATA != 0
                    || desired_access & FILE_APPEND_DATA != 0
                    || desired_access & GENERIC_WRITE != 0
                {
                    tracing::warn!(
                        path = unix_path,
                        "nt_create_file_hook: write mode not supported presently. falling back to original!"
                    );
                    return run_original();
                }
            }
        }

        // Check if pointer to handle is valid.
        if file_handle.is_null() {
            tracing::warn!("nt_create_file_hook: Invalid memory for file_handle variable in hook");
            return run_original();
        }

        if !is_memory_valid(io_status_block) {
            tracing::warn!(
                "nt_create_file_hook: Invalid memory for io_status_block variable in hook"
            );
            return run_original();
        }

        // Get open options.
        // NOTE(gabriela): currently read-only!
        // TODO(gabriela): update when write mode supported!
        let open_options = OpenOptionsInternal {
            read: true,
            ..Default::default()
        };

        // Try to open the file on the pod.
        let req = make_proxy_request_with_response(OpenFileRequest {
            path: PathBuf::from(&unix_path),
            open_options,
        });

        let managed_handle = match req {
            Ok(Ok(file)) => {
                let current_time = WindowsTime::current().as_file_time();

                try_insert_handle(HandleContext {
                    path: unix_path.clone(),
                    fd: file.fd,
                    desired_access,
                    file_attributes,
                    share_access,
                    create_disposition,
                    create_options,
                    creation_time: current_time,
                    access_time: current_time,
                    write_time: current_time,
                    change_time: current_time,
                })
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    ?e,
                    ?unix_path,
                    "nt_create_file_hook: Request for open file failed!",
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    ?e,
                    ?unix_path,
                    "nt_create_file_hook: Request for open file failed!",
                );
                None
            }
        };

        if let Some(handle) = managed_handle {
            // Write managed handle at the provided pointer
            *file_handle = *handle;
            tracing::info!(
                "nt_create_file_hook: Succesfully opened remote file handle for {} ({:8x})",
                unix_path,
                *file_handle as usize
            );
            STATUS_SUCCESS
        } else {
            // File could not be obtained for reasons, even if the
            // network operation succeeded.

            tracing::info!(
                ?unix_path,
                "nt_create_file_hook: Failed opening remote file handle"
            );

            run_original()
        }
    }
}

/// [`nt_read_file_hook`] is the function responsible for reading the contents of a remote file
/// via a previously acquired [`managed_handle::MirrordHandle`].
///
/// The mechanism is:
///
/// 1. We check if the `file` argument is present in [`MANAGED_HANDLES`], and acquire a write lock
///    over the [`HandleContext`].
///     * If `file` is not a managed handle, we fall through to the original `NtReadFile`
///       implementation.
///     * Both `buffer` and `io_status_block` are validated for pointer provenance at this point. If
///       either is invalid, we return [`STATUS_ACCESS_VIOLATION`].
/// 2. We update the [`HandleContext`] access time to the current time, as a read is taking place.
/// 3. We seek to the appropriate position on the remote file descriptor:
///     * If `byte_offset` is null, we issue a no-op seek (`Current(0)`) to obtain the current seek
///       head without moving it.
///     * If `byte_offset` is non-null, we seek to the absolute offset specified by its `QuadPart`
///       value, overwriting the current seek head.
///     * If the seek fails, we return [`STATUS_UNEXPECTED_NETWORK_ERROR`].
/// 4. We send a [`ReadFileRequest`] to the pod, requesting `length` bytes starting at the seek head
///    established in step 3.
///     * 4.1. If the request fails (network or protocol error), we return
///       [`STATUS_UNEXPECTED_NETWORK_ERROR`].
/// 5. We check if the returned byte buffer is empty, which signals EOF at the current seek head.
///     * If so, we fill `io_status_block` with [`STATUS_END_OF_FILE`] and return it.
/// 6. We calculate the maximum copyable length as `min(bytes.len(), length)`, then copy that many
///    bytes into the user-provided `buffer` - which was already validated for pointer provenance in
///    step 1 - and record the byte count in `io_status_block.Information`.
/// 7. We update or reset the remote seek head depending on the presence of `byte_offset`:
///     * If `byte_offset` is null (sequential read), we advance the seek head forward by the number
///       of bytes that were copied.
///     * If `byte_offset` is non-null (absolute read), we reset the seek head to the start of the
///       file.
///     * If this seek fails, we return [`STATUS_UNEXPECTED_NETWORK_ERROR`].
/// 8. We return [`STATUS_SUCCESS`].
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
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
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(mut handle_context) = managed_handle.clone().try_write()
        {
            if !is_memory_valid(buffer) {
                tracing::warn!("nt_read_file_hook: Invalid memory for buffer variable");
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                tracing::warn!("nt_read_file_hook: Invalid memory for io_status_block variable");
                return STATUS_ACCESS_VIOLATION;
            }

            // Update last access time to current time, as we are reading.
            handle_context.access_time = WindowsTime::current().as_file_time();

            // Get cursor, or update cursor if we have a byte offset.
            if try_seek(
                handle_context.fd,
                if byte_offset.is_null() {
                    SeekFromInternal::Current(0)
                } else {
                    SeekFromInternal::Start(*(*byte_offset).QuadPart() as _)
                },
            )
            .is_none()
            {
                tracing::error!(
                    fd = handle_context.fd,
                    path = handle_context.path,
                    byte_offset = if byte_offset.is_null() {
                        "null".to_string()
                    } else {
                        (*byte_offset).QuadPart().to_string()
                    },
                    "nt_read_file_hook: Failed seeking when reading file!"
                );
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            };

            let bytes = match make_proxy_request_with_response(ReadFileRequest {
                remote_fd: handle_context.fd,
                buffer_size: length as _,
            }) {
                Ok(Ok(res)) => Some(res.bytes),
                _ => None,
            };

            if let Some(bytes) = bytes {
                // Once there is no longer any bytes returned from the [`ReadFileRequest`] at
                // cursor, this signifies that we've reachced EOF.
                if bytes.is_empty() {
                    (*io_status_block).__bindgen_anon_1.Status =
                        ManuallyDrop::new(STATUS_END_OF_FILE);
                    (*io_status_block).Information = 0;

                    return STATUS_END_OF_FILE;
                }

                // Calculate maximum copyable size.
                let len = usize::min(bytes.len(), length as _);

                // Copy the remote received buffer (relative to fd cursor) to the provided buffer,
                // as far as we can.
                std::ptr::copy(bytes.as_ptr(), buffer as _, len);

                // We get this information over IRP normally, so need to replicate the
                // structure in usermode
                (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);
                (*io_status_block).Information = len as _;

                // Update or reset handle context cursor.
                if try_seek(
                    handle_context.fd,
                    if byte_offset.is_null() {
                        SeekFromInternal::Current(len as _)
                    } else {
                        SeekFromInternal::Start(0)
                    },
                )
                .is_some()
                {
                    return STATUS_SUCCESS;
                } else {
                    tracing::error!(
                        fd = handle_context.fd,
                        path = handle_context.path,
                        byte_offset = if byte_offset.is_null() {
                            "null".to_string()
                        } else {
                            (*byte_offset).QuadPart().to_string()
                        },
                        "nt_read_file_hook: Failed seeking when reading file!"
                    );
                    return STATUS_UNEXPECTED_NETWORK_ERROR;
                }
            }

            // We didn't acquire any bytes.
            tracing::error!("nt_read_file_hook: Pod did not return a buffer when reading file!");
            return STATUS_UNEXPECTED_NETWORK_ERROR;
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

/// [`nt_set_information_file_hook`] is responsible for changing the internal state of
/// [`HandleContext`], and also the remote file descriptor state, supporting a variety of entries
/// defined in the [`FILE_INFORMATION_CLASS`], but not all. The current supported entries are:
///
/// - [`FILE_INFORMATION_CLASS::FileBasicInformation`]
/// - [`FILE_INFORMATION_CLASS::FilePositionInformation`]
///
/// In the case of attempts to set information with the `file_information_class`
/// [`FILE_INFORMATION_CLASS::FilePositionInformation`], a request will be made to the pod, to
/// update the seek head.
///
/// This status will be reflected in the [`HandleContext`], but also through
/// `NtQueryInformationFile` (and therefore, `stat` POSIX operations.)
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
unsafe extern "system" fn nt_set_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(mut handle_context) = managed_handle.clone().try_write()
        {
            if !is_memory_valid(file_information) {
                tracing::warn!(
                    "nt_set_information_file_hook: Invalid memory for file_information variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                tracing::warn!(
                    "nt_set_information_file_hook: Invalid memory for io_status_block variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            // NOTE(gabriela): In testing, this has always been the IoStatusBlock result.
            (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);
            (*io_status_block).Information = 0;

            // The possible values are documented at the following link:
            // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntsetinformationfile
            match file_information_class {
                // A FILE_BASIC_INFORMATION structure. The caller must have opened the file
                // with the FILE_WRITE_ATTRIBUTES flag specified in the DesiredAccess parameter.
                FILE_INFORMATION_CLASS::FileBasicInformation => {
                    if handle_context.desired_access & (FILE_WRITE_ATTRIBUTES) != 0 {
                        // Length must be the same size as [`FILE_BASIC_INFORMATION`] length!
                        if length as usize != std::mem::size_of::<FILE_BASIC_INFORMATION>() {
                            return STATUS_ACCESS_VIOLATION;
                        }

                        let in_ptr = file_information as *const FILE_BASIC_INFORMATION;

                        /// If you specify a value of zero for any of the XxxTime members of the
                        /// FILE_BASIC_INFORMATION structure, the ZwSetInformationFile function
                        /// keeps a file's current setting for that time.
                        fn update_time(file_time: &mut FILETIME, new_time: _LARGE_INTEGER) {
                            unsafe {
                                if new_time.QuadPart != 0 {
                                    file_time.dwLowDateTime = new_time.u.LowPart;
                                    file_time.dwHighDateTime = new_time.u.HighPart as u32;
                                }
                            }
                        }
                        update_time(&mut handle_context.access_time, (*in_ptr).LastAccessTime);
                        update_time(&mut handle_context.write_time, (*in_ptr).LastWriteTime);
                        update_time(&mut handle_context.change_time, (*in_ptr).ChangeTime);

                        handle_context.file_attributes = (*in_ptr).FileAttributes;
                    }

                    return STATUS_SUCCESS;
                }
                // Change the current file information, which is stored in a
                // FILE_POSITION_INFORMATION structure.
                FILE_INFORMATION_CLASS::FilePositionInformation => {
                    // Length must be the same size as [`FILE_POSITION_INFORMATION`] length!
                    if length as usize != std::mem::size_of::<FILE_POSITION_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    // Cast file information pointer to FILE_POSITION_INFORMATION
                    let in_ptr = file_information as *const FILE_POSITION_INFORMATION;

                    // Set CurrentByteOffset from FILE_POSITION_INFORMATION to handle context.
                    if try_seek(
                        handle_context.fd,
                        SeekFromInternal::Start((*in_ptr).CurrentByteOffset.QuadPart as _),
                    )
                    .is_some()
                    {
                        return STATUS_SUCCESS;
                    } else {
                        tracing::error!(
                            "nt_set_information_file_hook: Failed seeking when updating file information!"
                        );
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                }
                _ => {
                    tracing::warn!(
                        path = handle_context.path,
                        "nt_set_information_file_hook: Trying to set for file_information_class: {:?}, but it is not implemented!",
                        file_information_class,
                    );
                }
            }

            // NOTE(gabriela): while there's no access violation in our case,
            // this is the expected result through the NT API.
            return STATUS_ACCESS_VIOLATION;
        }

        let original = NT_SET_INFORMATION_FILE_TYPE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            file_information_class,
        )
    }
}

/// [`nt_set_volume_information_file_hook`] is not implemented!
unsafe extern "system" fn nt_set_volume_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    fs_info_class: FSINFOCLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
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

/// [`nt_set_quota_information_file_hook`] is not implemented!
unsafe extern "system" fn nt_set_quota_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
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

/// [`nt_query_information_file_hook`] is the function responsible for querying the state of
/// [`HandleContext`] through the Nt API. This function supports a variety of the entries described
/// in the [`FILE_INFORMATION_CLASS`] enum, but not all. The current supported entries are:
///
/// - [`FILE_INFORMATION_CLASS::FileBasicInformation`]
/// - [`FILE_INFORMATION_CLASS::FilePositionInformation`]
/// - [`FILE_INFORMATION_CLASS::FileStandardInformation`]
/// - [`FILE_INFORMATION_CLASS::FileStatInformation`]
/// - [`FILE_INFORMATION_CLASS::FileAllInformation`]
///
/// In the case of trying to query information for a file with the `file_information_class` being
/// one of [`FILE_INFORMATION_CLASS::FilePositionInformation`] and consequently,
/// [`FILE_INFORMATION_CLASS::FileAllInformation`], a request will be made to the pod to query the
/// current seek head.
///
/// In the case of trying to query information for a file with the `file_information_class`
/// [`FILE_INFORMATION_CLASS::FileStatInformation`],
/// [`FILE_INFORMATION_CLASS::FileStandardInformation`] and consequently
/// [`FILE_INFORMATION_CLASS::FileAllInformation`], a request will be made to the pod to query
/// the content of the `stat` operation.
///
/// This information will be reflected, for example, during the invocation of the `stat` POSIX API.
/// The path for that event, despite one of the misleading names above, is the
/// [`FILE_INFORMATION_CLASS::FileAllInformation`] match arm.
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
unsafe extern "system" fn nt_query_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
        {
            if !is_memory_valid(file_information) {
                tracing::warn!(
                    "nt_query_information_file_hook: Invalid memory for file_information variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                tracing::warn!(
                    "nt_query_information_file_hook: Invalid memory for io_status_block variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            match file_information_class {
                // A FILE_BASIC_INFORMATION structure. The caller must have opened the file with
                // the FILE_READ_ATTRIBUTES flag specified in the DesiredAccess parameter.
                FILE_INFORMATION_CLASS::FileBasicInformation => {
                    if handle_context.desired_access & FILE_READ_ATTRIBUTES != 0 {
                        // Length must be the same size as [`FILE_BASIC_INFORMATION`] length!
                        if length as usize != std::mem::size_of::<FILE_BASIC_INFORMATION>() {
                            return STATUS_ACCESS_VIOLATION;
                        }

                        let out_ptr = file_information as *mut FILE_BASIC_INFORMATION;

                        // Initialize structure from our context.
                        (*out_ptr).CreationTime.QuadPart =
                            WindowsTime::from(handle_context.creation_time).as_file_time_i64();
                        (*out_ptr).LastAccessTime.QuadPart =
                            WindowsTime::from(handle_context.access_time).as_file_time_i64();
                        (*out_ptr).LastWriteTime.QuadPart =
                            WindowsTime::from(handle_context.write_time).as_file_time_i64();
                        (*out_ptr).ChangeTime.QuadPart =
                            WindowsTime::from(handle_context.change_time).as_file_time_i64();
                        (*out_ptr).FileAttributes = handle_context.file_attributes;

                        (*io_status_block).Information =
                            std::mem::size_of::<FILE_BASIC_INFORMATION>() as _;
                        (*io_status_block).__bindgen_anon_1.Status =
                            ManuallyDrop::new(STATUS_SUCCESS);
                    } else {
                        tracing::warn!(
                            "nt_query_information_file_hook: Trying to query FileBasicInformation with the wrong desired_access"
                        );
                    }

                    return STATUS_SUCCESS;
                }
                // A FILE_POSITION_INFORMATION structure. The caller must have opened the file with
                // the DesiredAccess FILE_READ_DATA or FILE_WRITE_DATA flag
                // specified in the DesiredAccess parameter.
                //
                //
                // NOTE(gabriela): the above is wrong and stupid, you just need read-attribs.
                FILE_INFORMATION_CLASS::FilePositionInformation => {
                    if handle_context.desired_access & FILE_READ_ATTRIBUTES != 0 {
                        // Length must be the same size as [`FILE_POSITION_INFORMATION`] length!
                        if length as usize != std::mem::size_of::<FILE_POSITION_INFORMATION>() {
                            return STATUS_ACCESS_VIOLATION;
                        }

                        let out_ptr = file_information as *mut FILE_POSITION_INFORMATION;

                        if let Some(cursor) =
                            try_seek(handle_context.fd, SeekFromInternal::Current(0))
                        {
                            (*out_ptr).CurrentByteOffset.QuadPart = cursor as _;

                            (*io_status_block).Information =
                                std::mem::size_of::<FILE_POSITION_INFORMATION>() as _;
                            (*io_status_block).__bindgen_anon_1.Status =
                                ManuallyDrop::new(STATUS_SUCCESS);

                            return STATUS_SUCCESS;
                        } else {
                            tracing::error!(
                                "nt_query_information_file_hook: Failed seeking when querying file information!"
                            );
                            return STATUS_UNEXPECTED_NETWORK_ERROR;
                        }
                    } else {
                        tracing::warn!(
                            "nt_query_information_file_hook: Trying to query FilePositionInformation with the wrong desired_access"
                        );
                    }
                }
                // A FILE_STANDARD_INFORMATION structure. The caller can query this information as
                // long as the file is open, without any particular requirements for
                // DesiredAccess.
                FILE_INFORMATION_CLASS::FileStandardInformation => {
                    // Length must be the same size as [`FILE_STANDARD_INFORMATION`] length!
                    if length as usize != std::mem::size_of::<FILE_STANDARD_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    let out_ptr = file_information as *mut FILE_STANDARD_INFORMATION;

                    if let Some(metadata) = try_xstat(handle_context.fd) {
                        (*out_ptr).AllocationSize.QuadPart = i64::try_from(metadata.size).unwrap();
                        (*out_ptr).EndOfFile.QuadPart = i64::try_from(metadata.size).unwrap();
                        (*out_ptr).NumberOfLinks = 0;
                        (*out_ptr).DeletePending = FALSE;
                        (*out_ptr).Directory = FALSE;

                        (*io_status_block).Information =
                            std::mem::size_of::<FILE_STANDARD_INFORMATION>() as _;
                        (*io_status_block).__bindgen_anon_1.Status =
                            ManuallyDrop::new(STATUS_SUCCESS);

                        return STATUS_SUCCESS;
                    } else {
                        tracing::error!(
                            "nt_query_information_file_hook: Failed xstat when querying file information!"
                        );
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                }
                // https://github.com/reactos/reactos/blob/9ab8761f2c76d437830195ebea2600e414ac4c73/dll/win32/kernelbase/wine/file.c#L3039
                // Doesn't seem to be used in Windows 11
                FILE_INFORMATION_CLASS::FileStatInformation => {
                    // NOTE(gabriela): tracking @ https://github.com/delulu-hq/phnt-rs/issues/7
                    // let's remove once the bindgen of above supports it
                    // i just quickly did it myself by hand for now
                    #[allow(non_camel_case_types)]
                    #[repr(C)]
                    struct FILE_STAT_INFORMATION {
                        FileId: _LARGE_INTEGER,
                        CreationTime: _LARGE_INTEGER,
                        LastAccessTime: _LARGE_INTEGER,
                        LastWriteTime: _LARGE_INTEGER,
                        ChangeTime: _LARGE_INTEGER,
                        AllocationSize: _LARGE_INTEGER,
                        EndOfFile: _LARGE_INTEGER,
                        FileAttributes: ULONG,
                        ReparseTag: ULONG,
                        NumberOfLinks: ULONG,
                        EffectiveAccess: ACCESS_MASK,
                    }

                    // Length must be the same size as [`FILE_STAT_INFORMATION`] length!
                    if length as usize != std::mem::size_of::<FILE_STAT_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    let out_ptr = file_information as *mut FILE_STAT_INFORMATION;

                    if let Some(metadata) = try_xstat(handle_context.fd) {
                        (*out_ptr).FileId.QuadPart = 0;
                        (*out_ptr).FileAttributes = handle_context.file_attributes;
                        (*out_ptr).CreationTime.QuadPart =
                            WindowsTime::from(handle_context.creation_time).as_file_time_i64();
                        (*out_ptr).LastAccessTime.QuadPart =
                            WindowsTime::from(handle_context.access_time).as_file_time_i64();
                        (*out_ptr).LastWriteTime.QuadPart =
                            WindowsTime::from(handle_context.write_time).as_file_time_i64();
                        (*out_ptr).ChangeTime.QuadPart =
                            WindowsTime::from(handle_context.change_time).as_file_time_i64();
                        (*out_ptr).AllocationSize.QuadPart = i64::try_from(metadata.size).unwrap();
                        (*out_ptr).EndOfFile.QuadPart = i64::try_from(metadata.size).unwrap();
                        (*out_ptr).ReparseTag = 0;
                        (*out_ptr).NumberOfLinks = 0;
                        (*out_ptr).EffectiveAccess = handle_context.desired_access;

                        (*io_status_block).Information =
                            std::mem::size_of::<FILE_STAT_INFORMATION>() as _;
                        (*io_status_block).__bindgen_anon_1.Status =
                            ManuallyDrop::new(STATUS_SUCCESS);

                        return STATUS_SUCCESS;
                    } else {
                        tracing::error!(
                            "nt_query_information_file_hook: Failed xstat when querying file information!"
                        );
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                }
                // NOTE(gabriela): well, this works as far as `GetFileInformationByHandle` is
                // concerned, at least.
                FILE_INFORMATION_CLASS::FileAllInformation => {
                    // Length must be the same size as [`FILE_ALL_INFORMATION`] length!
                    if length as usize != std::mem::size_of::<FILE_ALL_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    let out_ptr = file_information as *mut FILE_ALL_INFORMATION;

                    fn query_single<T: Default>(
                        handle: HANDLE,
                        kind: FILE_INFORMATION_CLASS,
                    ) -> Option<T> {
                        let mut single = T::default();
                        let mut io_status_block = _IO_STATUS_BLOCK::default();

                        let res = unsafe {
                            nt_query_information_file_hook(
                                handle,
                                &mut io_status_block,
                                std::ptr::from_mut(&mut single) as _,
                                std::mem::size_of::<T>() as _,
                                kind,
                            )
                        };
                        if res != STATUS_SUCCESS
                            || unsafe { io_status_block.__bindgen_anon_1.Status }
                                != ManuallyDrop::new(STATUS_SUCCESS)
                        {
                            return None;
                        }

                        if std::mem::size_of::<T>() != io_status_block.Information as usize {
                            return None;
                        }

                        Some(single)
                    }

                    fn query_all(handle: HANDLE) -> Option<FILE_ALL_INFORMATION> {
                        let all_info = FILE_ALL_INFORMATION {
                            BasicInformation: query_single(
                                handle,
                                FILE_INFORMATION_CLASS::FileBasicInformation,
                            )?,
                            StandardInformation: query_single(
                                handle,
                                FILE_INFORMATION_CLASS::FileStandardInformation,
                            )?,
                            PositionInformation: query_single(
                                handle,
                                FILE_INFORMATION_CLASS::FilePositionInformation,
                            )?,
                            ..Default::default()
                        };

                        Some(all_info)
                    }

                    if let Some(all_info) = query_all(file) {
                        (*out_ptr) = all_info;

                        (*io_status_block).Information =
                            std::mem::size_of::<FILE_ALL_INFORMATION>() as _;
                        (*io_status_block).__bindgen_anon_1.Status =
                            ManuallyDrop::new(STATUS_SUCCESS);

                        return STATUS_SUCCESS;
                    } else {
                        tracing::error!(
                            "nt_query_information_file_hook: Failed querying all information because one of the single queries failed!"
                        );
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                }
                _ => {
                    tracing::warn!(
                        path = handle_context.path,
                        "nt_query_information_file_hook: Trying to query for file_information_class: {:?}, but it is not implemented!",
                        file_information_class
                    );
                }
            }

            // NOTE(gabriela): while there's no access violation in our case,
            // this is the expected result through the NT API.
            return STATUS_ACCESS_VIOLATION;
        }

        let original = NT_QUERY_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            file_information_class,
        )
    }
}

/// [`nt_query_attributes_file_hook`] is unimplemented!
unsafe extern "system" fn nt_query_attributes_file_hook(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe {
        for_each_handle_with_path(object_attributes, |handle, handle_context| {
            tracing::warn!(
                path = handle_context.path,
                "nt_query_attributes_file_hook: Function not implemented! (handle: {:08x})",
                handle.0 as usize
            );
        });

        let original = NT_QUERY_ATTRIBUTES_FILE_ORIGINAL.get().unwrap();
        original(object_attributes, file_basic_info)
    }
}

/// [`nt_query_volume_information_file_hook`] is the function responsible for querying
/// the volume on which a file we manage finds itself in. The type of information being queried
/// is distinguished by the [`FSINFOCLASS`] value. The current supported entries are:
///
/// - [`FSINFOCLASS::FileFsDeviceInformation`]
/// - [`FSINFOCLASS::FileFsVolumeInformation`]
///
/// This is responsible to support functions such as `kernelbase!GetFileType`.
unsafe extern "system" fn nt_query_volume_information_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    fs_info_class: FSINFOCLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
        {
            if !is_memory_valid(file_information) {
                tracing::warn!(
                    "nt_query_volume_information_file_hook: Invalid memory for file_information variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                tracing::warn!(
                    "nt_query_volume_information_file_hook: Invalid memory for io_status_block variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            match fs_info_class {
                FSINFOCLASS::FileFsDeviceInformation => {
                    // Length must be the same size as [`FILE_FS_DEVICE_INFORMATION`] length!
                    if length as usize != std::mem::size_of::<FILE_FS_DEVICE_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    let out_ptr = file_information as *mut FILE_FS_DEVICE_INFORMATION;

                    // NOTE(gabriela): kernelbase!GetFileType says DeviceType
                    // 36 (according to ntddk.h, FILE_DEVICE_VIRTUAL_DISK (0x24)) returns
                    // the FILE_TYPE 1 (FILE_TYPE_DISK) which has worked this far.
                    (*out_ptr).DeviceType = FILE_DEVICE_VIRTUAL_DISK;
                    // NOTE(gabriela): update once write support!
                    (*out_ptr).Characteristics = FILE_READ_ONLY_DEVICE;

                    (*io_status_block).Information =
                        std::mem::size_of::<FILE_FS_DEVICE_INFORMATION>() as _;
                    (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);

                    return STATUS_SUCCESS;
                }
                FSINFOCLASS::FileFsVolumeInformation => {
                    // Length must be the same size as [`FILE_FS_VOLUME_INFORMATION`] length!
                    // NOTE: or must it? `VolumeLabelLength` is dynamic
                    //
                    // "The size of the buffer passed in the FileInformation parameter to
                    // FltQueryVolumeInformation or ZwQueryVolumeInformationFile
                    // must be at least sizeof (FILE_FS_VOLUME_INFORMATION)."
                    // - https://ntdoc.m417z.com/file_fs_volume_information
                    //
                    // ... but for now, this is enough to support Node.
                    if length as usize != std::mem::size_of::<FILE_FS_VOLUME_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    let current_time = WindowsTime::current().as_file_time();

                    let out_ptr = file_information as *mut FILE_FS_VOLUME_INFORMATION;
                    (*out_ptr).VolumeCreationTime.u.LowPart = current_time.dwLowDateTime;
                    (*out_ptr).VolumeCreationTime.u.HighPart = current_time.dwHighDateTime as _;

                    (*out_ptr).VolumeSerialNumber = 0u32;

                    // NOTE: this *can* be dynamic, and if you look at the struct definition, it's a
                    // classic case of `_Field_size_bytes_(VolumeLabelLength)`
                    (*out_ptr).VolumeLabelLength = 1;

                    // what the hell is a "object-oriented file system objects" ???
                    (*out_ptr).SupportsObjects = FALSE;

                    // Just tell the person doing the query that it's on C:/.
                    // Everything's on C:/.
                    (*out_ptr).VolumeLabel = ['C' as u16];

                    (*io_status_block).Information =
                        std::mem::size_of::<FILE_FS_VOLUME_INFORMATION>() as _;
                    (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);

                    return STATUS_SUCCESS;
                }
                _ => {
                    tracing::warn!(
                        path = handle_context.path,
                        "nt_query_volume_information_file_hook: Trying to query for FSINFOCLASS: {:?}, but it is not implemented!",
                        fs_info_class
                    );
                }
            }

            // NOTE(gabriela): while there's no access violation in our case,
            // this is the expected result through the NT API.
            return STATUS_ACCESS_VIOLATION;
        }

        let original = NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            fs_info_class,
        )
    }
}

/// [`nt_query_quota_information_file_hook`] is unimplemented!
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
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
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

/// [`nt_delete_file_hook`] is unimplemented!
unsafe extern "system" fn nt_delete_file_hook(object_attributes: POBJECT_ATTRIBUTES) -> NTSTATUS {
    unsafe {
        for_each_handle_with_path(object_attributes, |handle, handle_context| {
            tracing::warn!(
                path = handle_context.path,
                "nt_delete_file_hook: Attempt to delete file that current handle ({:8x}) points to! Not implemented! Will fall back on original!",
                handle.0 as usize
            );
        });

        let original = NT_DELETE_FILE_ORIGINAL.get().unwrap();
        original(object_attributes)
    }
}

/// [`nt_device_io_control_file_hook`] is unimplemented!
/// NOTE(gabriela): SUPER unsafe to print in!
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
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(_handle_context) = managed_handle.clone().try_read()
        {}

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

/// [`nt_lock_file_hook`] is unimplemented!
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
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
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

/// [`nt_unlock_file_hook`] is unimplemented!
unsafe extern "system" fn nt_unlock_file_hook(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    byte_offset: PLARGE_INTEGER,
    length: PLARGE_INTEGER,
    key: ULONG,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file)
            && let Ok(handle_context) = managed_handle.clone().try_read()
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

/// [`nt_close_hook`] is the function responsible for signaling to the remote pod that the file
/// descriptor can now be closed, and it is also responsible of removing the [`HandleContext`] for
/// `handle` from the [`MANAGED_HANDLES`] structure.
///
/// As a result, all future operations done on our [`HANDLE`] will be invalid.
///
/// Due to details of the [`MANAGED_HANDLES`] structure management, a [`HANDLE`] value can never be
/// reclaimed, so all cases of reusage must be treated as a bug.
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
unsafe extern "system" fn nt_close_hook(handle: HANDLE) -> NTSTATUS {
    unsafe {
        if let Ok(mut handles) = MANAGED_HANDLES.try_write()
            && let Some(managed_handle) = handles.get(&handle)
            && let Ok(handle_context) = managed_handle.clone().try_read()
        {
            let req = make_proxy_request_no_response(CloseFileRequest {
                fd: handle_context.fd,
            });

            // Remove entry regardless of following response.
            handles.remove_entry(&handle);

            if req.is_err() {
                // Valid [`NTSTATUS`] to facillitate investigations later.
                tracing::error!("nt_close_hook: Failed closing fd when closing file handle!");
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            }

            tracing::info!(
                "nt_close_hook: Succesfully closed handle {:8x}",
                handle as usize
            );

            return STATUS_SUCCESS;
        }

        let original = NT_CLOSE_ORIGINAL.get().unwrap();
        original(handle)
    }
}

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
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
        "NtSetInformationFile",
        nt_set_information_file_hook,
        NtSetInformationFileType,
        NT_SET_INFORMATION_FILE_TYPE_ORIGINAL
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

//! Module responsible for registering hooks targetting file operations syscalls.

use std::{
    ffi::c_void,
    mem::ManuallyDrop,
    path::PathBuf,
    sync::OnceLock,
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::proxy_connection::{
    make_proxy_request_no_response, make_proxy_request_with_response,
};
use mirrord_protocol::file::{
    CloseFileRequest, OpenFileRequest, OpenOptionsInternal, ReadFileRequest, SeekFromInternal,
};
use phnt::ffi::{
    _IO_STATUS_BLOCK, _LARGE_INTEGER, FILE_ALL_INFORMATION,
    FILE_BASIC_INFORMATION, FILE_FS_DEVICE_INFORMATION, FILE_INFORMATION_CLASS,
    FILE_POSITION_INFORMATION, FILE_READ_ONLY_DEVICE, FILE_STANDARD_INFORMATION, FSINFOCLASS, PFILE_BASIC_INFORMATION, PIO_APC_ROUTINE,
};
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
            WindowsTime, read_object_attributes_name, remove_root_dir_from_path, try_seek,
            try_xstat,
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
    *mut c_void,
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
/// - We try to run the `NtCreateFile` operation normally initially. Only should it fail with
///   [`STATUS_OBJECT_PATH_NOT_FOUND`], then do we check if the path could possibly be a Linux path.
/// - If the path is a Linux path, then we send an open request to the pod
/// - And if it is succesful, we work to replicate the internal behavior of the `NtCreateFile`
///   function, creating the same expectations for input,
///
/// Internally, this creates a "managed handle" (see [`MANAGED_HANDLES`]), and if there was a
/// succesful open operation, we create a [`HandleContext`] for the file descriptor, which is
/// initially filled in with relevant data, and may surely be modified later.
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn nt_create_file_hook(
    file_handle: PHANDLE,
    desired_access: ACCESS_MASK,
    object_attributes: POBJECT_ATTRIBUTES,
    io_status_block: *mut c_void,
    allocation_size: PLARGE_INTEGER,
    file_attributes: ULONG,
    share_access: ULONG,
    create_disposition: ULONG,
    create_options: ULONG,
    ea_buf: PVOID,
    ea_size: ULONG,
) -> NTSTATUS {
    unsafe {
        let original = NT_CREATE_FILE_ORIGINAL.get().unwrap();
        let ret = original(
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
        );

        let name = read_object_attributes_name(object_attributes);

        // This is usually the case when a Linux path is provided.
        if ret == STATUS_OBJECT_PATH_NOT_FOUND {
            // Try to get path.
            let linux_path = remove_root_dir_from_path(name);

            // If no path, return.
            if linux_path.is_none() {
                return ret;
            }
            let linux_path = linux_path.unwrap();
            if desired_access & FILE_WRITE_DATA != 0
                || desired_access & FILE_APPEND_DATA != 0
                || desired_access & GENERIC_WRITE != 0
            {
                // TODO(gabriela): edit when supported!
                return STATUS_OBJECT_PATH_NOT_FOUND;
            }

            // Check if pointer to handle is valid.
            if file_handle.is_null() {
                tracing::warn!(
                    "nt_create_file_hook: Invalid memory for file_handle variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                tracing::warn!(
                    "nt_create_file_hook: Invalid memory for io_status_block variable in hook"
                );
                return STATUS_ACCESS_VIOLATION;
            }

            // Get open options.
            let open_options = OpenOptionsInternal {
                read: true,
                ..Default::default()
            };

            // Try to open the file on the pod.
            let Ok(req) = make_proxy_request_with_response(OpenFileRequest {
                path: PathBuf::from(&linux_path),
                open_options,
            }) else {
                tracing::error!("nt_create_file_hook: Request for open file failed!");
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            };

            let managed_handle = match req {
                Ok(file) => {
                    let current_time = WindowsTime::current().as_file_time();

                    try_insert_handle(HandleContext {
                        path: linux_path.clone(),
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
                Err(e) => {
                    tracing::warn!("nt_create_file_hook: {:?} {}", e, linux_path.clone());
                    None
                }
            };

            if managed_handle.is_some() {
                // Write managed handle at the provided pointer
                *file_handle = *managed_handle.unwrap();
                tracing::info!(
                    "nt_create_file_hook: Succesfully opened remote file handle for {} ({:8x})",
                    linux_path,
                    *file_handle as usize
                );
                return STATUS_SUCCESS;
            } else {
                // File could not be obtained for reasons, even if the
                // network operation succeeded.

                tracing::info!(
                    "nt_create_file_hook: Failed opening remote file handle for {}",
                    linux_path
                );
                return STATUS_OBJECT_PATH_NOT_FOUND;
            }
        }

        ret
    }
}

/// [`nt_read_file_hook`] is the function responsible for reading the contents of the acquired
/// file descriptor from the pod.
///
/// The mechanism is:
/// - We check if the `file` argument is present in the [`MANAGED_HANDLES`] structure, and, if it
///   is, we then begin taking over execution.
/// - We try to, either, acquire the current seek head, or update to the user-desired seek head,
///   which may be specified by the `byte_offset` structure's contents.
/// - Once a buffer is obtained from the pod, it is matching the current seek head of the file, and
///   the desired length to be read, we copy the slice we made over our newly returned buffer, to
///   the user-provided buffer, should all the preconditions for this be met.
/// - If there is not a specified `byte_offset`, we assume the intent is to call the API until we've
///   run out of content to read. Therefore, we seek from current with the calculated length that
///   was copied to the user provided buffer.
/// - Otherwise, in the presence of a `byte_offset` structure, the seek head is reset. It can also
///   be reset by `NtSetInformationFile` using `FilePositionInfo`.
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
            let Some(cursor) = try_seek(
                handle_context.fd,
                if byte_offset.is_null() {
                    SeekFromInternal::Current(0)
                } else {
                    SeekFromInternal::Start(*(*byte_offset).QuadPart() as _)
                },
            ) else {
                tracing::error!("nt_read_file_hook: Failed seeking when reading file!");
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
                if cursor as usize >= bytes.len() {
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
                    tracing::error!("nt_read_file_hook: Failed seeking when reading file!");
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

#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                        "nt_set_information_file_hook: Trying to set for file_information_class: {:?}, but it is not implemented! (file: {})",
                        file_information_class,
                        &handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                "nt_set_volume_information_file_hook: Not implemented! Failling back on original! (file: {}, FSINFOCLASS: {:?})",
                &handle_context.path,
                fs_info_class
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                "nt_set_quota_information_file_hook: Not implemented! Failling back on original! (file: {})",
                &handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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

                    fn query_single<T: Default>(handle: HANDLE, kind: FILE_INFORMATION_CLASS) -> Option<T> {
                        let mut single = T::default();
                        let mut io_status_block =_IO_STATUS_BLOCK::default();

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
                        let mut all_info = FILE_ALL_INFORMATION::default();

                        all_info.BasicInformation =
                            query_single(handle, FILE_INFORMATION_CLASS::FileBasicInformation)?;
                        all_info.StandardInformation =
                            query_single(handle, FILE_INFORMATION_CLASS::FileStandardInformation)?;
                        all_info.PositionInformation =
                            query_single(handle, FILE_INFORMATION_CLASS::FilePositionInformation)?;

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
                        "nt_query_information_file_hook: Trying to query for file_information_class: {:?}, but it is not implemented! (file: {})",
                        file_information_class,
                        handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn nt_query_attributes_file_hook(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe {
        for_each_handle_with_path(object_attributes, |handle, handle_context| {
            tracing::warn!(
                "nt_query_attributes_file_hook: Function not implemented! (handle: {:08x}, file: {}) ",
                handle.0 as usize,
                &handle_context.path
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
///
/// This is responsible to support functions such as `kernelbase!GetFileType`.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                _ => {
                    tracing::warn!(
                        "nt_query_volume_information_file_hook: Trying to query for FSINFOCLASS: {:?}, but it is not implemented! (file: {})",
                        fs_info_class,
                        &handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                "nt_query_quota_information_file_hook: Not implemented! Failling back on original! (file: {})",
                &handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn nt_delete_file_hook(object_attributes: POBJECT_ATTRIBUTES) -> NTSTATUS {
    unsafe {
        for_each_handle_with_path(object_attributes, |handle, handle_context| {
            tracing::warn!(
                "nt_delete_file_hook: Attempt to delete file that current handle ({:8x}) points to! Not implemented! Will fall back on original! (file: {})",
                handle.0 as usize,
                &handle_context.path
            );
        });

        let original = NT_DELETE_FILE_ORIGINAL.get().unwrap();
        original(object_attributes)
    }
}

/// [`nt_device_io_control_file_hook`] is unimplemented!
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
            && let Ok(handle_context) = managed_handle.clone().try_read()
        {
            tracing::warn!(
                "nt_device_io_control_file_hook: Not implemented! Failling back on original! (file: {})",
                &handle_context.path
            );
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

/// [`nt_lock_file_hook`] is unimplemented!
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                "nt_lock_file_hook: Not implemented! Failling back on original! (file: {})",
                &handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
                "nt_unlock_file_hook: Not implemented! Failling back on original! (file: {})",
                &handle_context.path
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
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
        "NtReadFile",
        nt_read_file_hook,
        NtReadFileType,
        NT_READ_FILE_ORIGINAL
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

    apply_hook!(
        guard,
        "ntdll",
        "NtClose",
        nt_close_hook,
        NtCloseType,
        NT_CLOSE_ORIGINAL
    )?;

    // ----------------------------------------------------------------------------
    // NOTE(gabriela): the following hooks are unimplemented!
    // They're only here to trace missing logic! Please move above once they're
    // implemented!

    apply_hook!(
        guard,
        "ntdll",
        "NtWriteFile",
        nt_write_file_hook,
        NtWriteFileType,
        NT_WRITE_FILE_ORIGINAL
    )?;

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

    // ----------------------------------------------------------------------------

    Ok(())
}

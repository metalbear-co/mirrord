//! Module responsible for registering hooks targetting file operations syscalls.

use std::{
    ffi::c_void,
    mem::{ManuallyDrop, MaybeUninit},
    path::PathBuf,
    sync::OnceLock,
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::proxy_connection::{
    make_proxy_request_no_response, make_proxy_request_with_response,
};
use mirrord_protocol::file::{
    CloseFileRequest, OpenFileRequest, OpenOptionsInternal, ReadFileRequest,
};
use phnt::ffi::{
    _IO_STATUS_BLOCK, _LARGE_INTEGER, FILE_ALL_INFORMATION, FILE_BASIC_INFORMATION,
    FILE_INFORMATION_CLASS, FILE_POSITION_INFORMATION, FILE_STANDARD_INFORMATION, IO_STATUS_BLOCK,
    PFILE_BASIC_INFORMATION,
};
use str_win::u16_buffer_to_string;
use winapi::{
    shared::{
        minwindef::{DWORD, FILETIME},
        ntdef::{
            HANDLE, NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID, ULONG,
        },
        ntstatus::{
            STATUS_ACCESS_VIOLATION, STATUS_END_OF_FILE, STATUS_OBJECT_PATH_NOT_FOUND,
            STATUS_SUCCESS, STATUS_UNEXPECTED_NETWORK_ERROR,
        },
    },
    um::{
        winbase::FILE_TYPE_DISK,
        winnt::{
            ACCESS_MASK, FILE_APPEND_DATA, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
            FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, GENERIC_WRITE,
        },
    },
};

use crate::{
    apply_hook,
    hooks::files::{
        managed_handle::{HandleContext, MANAGED_HANDLES, try_insert_handle},
        util::{WindowsTime, remove_root_dir_from_path, try_xstat},
    },
    process::memory::is_memory_valid,
};

pub mod managed_handle;
pub mod util;

/// Function responsible for turning a [`OBJECT_ATTRIBUTES`] structure into a [`String`].
fn read_object_attributes_name(object_attributes: POBJECT_ATTRIBUTES) -> String {
    unsafe {
        let name_ustr = (*object_attributes).ObjectName;

        let buf = (*name_ustr).Buffer;
        let len = (*name_ustr).Length;

        let name = &*std::ptr::slice_from_raw_parts(buf, len as _);
        u16_buffer_to_string(name)
    }
}

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

type NtQueryVolumeInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut c_void,
    PVOID,
    ULONG,
    FILE_INFORMATION_CLASS,
) -> NTSTATUS;
static NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryVolumeInformationFileType> =
    OnceLock::new();

type NtCloseType = unsafe extern "system" fn(HANDLE) -> NTSTATUS;
static NT_CLOSE_ORIGINAL: OnceLock<&NtCloseType> = OnceLock::new();

type GetFileTypeType = unsafe extern "system" fn(HANDLE) -> DWORD;
static GET_FILE_TYPE_ORIGINAL: OnceLock<&GetFileTypeType> = OnceLock::new();

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
/// Another detail is that we make a xstat request to the remote pod to acquire the size of the
/// file.
///
/// All instances of a failed network operation are marked by the return value of
/// [`STATUS_UNEXPECTED_NETWORK_ERROR`].
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
            if desired_access & (FILE_WRITE_DATA) != 0
                || desired_access & (FILE_APPEND_DATA) != 0
                || desired_access & (GENERIC_WRITE) != 0
            {
                // TODO(gabriela): edit when supported!
                return STATUS_OBJECT_PATH_NOT_FOUND;
            }

            // Check if pointer to handle is valid.
            if file_handle.is_null() {
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                return STATUS_ACCESS_VIOLATION;
            }

            // Get open options.
            let open_options = OpenOptionsInternal {
                read: true,
                ..Default::default()
            };

            // Try to open the file on the pod.
            let req = make_proxy_request_with_response(OpenFileRequest {
                path: PathBuf::from(&linux_path),
                open_options,
            });

            // Try to get request.
            if req.is_err() {
                eprintln!("WARNING: request for open file failed!");
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            }
            let req = req.unwrap();

            let managed_handle = match req {
                Ok(file) => {
                    // Run Xstat over fd to get necessary data.
                    let xstat = try_xstat(file.fd);

                    if xstat.is_none() {
                        eprintln!("WARNING: request for xstat file failed!");
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                    let xstat = xstat.unwrap();

                    try_insert_handle(HandleContext {
                        path: linux_path,
                        fd: file.fd,
                        desired_access,
                        file_attributes,
                        share_access,
                        create_disposition,
                        create_options,
                        // Initialize following variables to current time. Update later.
                        creation_time: WindowsTime::current().as_file_time(),
                        access_time: WindowsTime::current().as_file_time(),
                        write_time: WindowsTime::current().as_file_time(),
                        change_time: WindowsTime::current().as_file_time(),
                        // Data will be retrieved on first read.
                        data: None,
                        // Xstat request contains the size of the remote file in this field,
                        data_xstat_size: xstat.size as _,
                        // Cursor is a detail to be maintained by reader, etc. Starts at 0.
                        cursor: 0,
                    })
                }
                Err(e) => {
                    eprintln!("{:?} {}", e, linux_path);
                    None
                }
            };

            if managed_handle.is_some() {
                // Write managed handle at the provided pointer
                *file_handle = *managed_handle.unwrap();
                println!(
                    "Succesfully wrote managed handle! {:8p}",
                    *managed_handle.unwrap()
                );
                return STATUS_SUCCESS;
            } else {
                // File could not be obtained for reasons, even if the
                // network operation succeeded.

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
/// - Upon the first [`nt_read_file_hook`] request for the file handle, if some conditions are met,
///   we make a request to read the entirety of the file descriptor into the [`HandleContext`], so
///   that later it may be referred to again for various read configurations. This is a necessity,
///   because otherwise, we'd have to, depending on the scenario, begin with a request to `seek` to
///   the point in the file we're trying to read at. Moreso, it could be seen, depending on the
///   context, as an optimizaqtion, to not throttle IO operations so much by doing a web request
///   every time. This is still subject to change.
/// - Once a buffer is obtained from the internal data, matching the beginning point in the file,
///   and the desired length to be read, we copy the slice we made over our internal data, to the
///   user-provided buffer, should all the preconditions for this be met.
/// - If there is not a specified `byte_offset`, we assume the intent is to call the API until we've
///   run out of content to read. Therefore, we maintain an internal cursor to which the minimum of
///   `length` and remaining data in the [`HandleContext`] is added.
/// - The internal cursor is reset if you do a read with a `byte_offset`. It can also be reset by
///   `NtSetInformationFile` using `FilePositionInfo`.
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
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                return STATUS_ACCESS_VIOLATION;
            }

            // If data is not present yet, read it once.
            // NOTE(gabriela): should we either
            // - do this
            // - update to seek + read every time ReadFile happens?
            if handle_context.data.is_none() {
                let req = make_proxy_request_with_response(ReadFileRequest {
                    remote_fd: handle_context.fd,
                    buffer_size: handle_context.data_xstat_size as _,
                });

                if req.is_err() {
                    return STATUS_UNEXPECTED_NETWORK_ERROR;
                }

                let req = req.unwrap();

                match req {
                    Ok(res) => {
                        handle_context.data = Some(res.bytes.to_vec());
                    }
                    Err(e) => {
                        eprintln!("{:?}", e);
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                }
            }

            // Update last access time to current time, as we are reading.
            handle_context.access_time = WindowsTime::current().as_file_time();

            // Data must be guaranteed by the time we get here!
            let bytes = handle_context.data.as_ref().unwrap();

            if handle_context.cursor >= bytes.len() {
                (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_END_OF_FILE);
                (*io_status_block).Information = 0;

                return STATUS_END_OF_FILE;
            }

            // Start offset if there's a byte offset.
            let start_off = if byte_offset.is_null() {
                handle_context.cursor
            } else {
                *(*byte_offset).QuadPart() as _
            };

            // Get a slice of the bytes.
            let bytes = &bytes[start_off..];

            let len = usize::min(bytes.len(), length as _);

            // Otherwise, copy the remote received buffer (relative to
            // fd cursor) to the provided buffer, as far as we can.
            std::ptr::copy(bytes.as_ptr(), buffer as _, len);

            // We get this information over IRP normally, so need to replicate the
            // structure in usermode
            (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);
            (*io_status_block).Information = len as _;

            // Update or reset handle context cursor.
            if byte_offset.is_null() {
                handle_context.cursor += len;
            } else {
                handle_context.cursor = 0;
            }

            return STATUS_SUCCESS;
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
/// [`HandleContext`], supporting a variety of entries defined in the [`FILE_INFORMATION_CLASS`],
/// but not all. The current supported entries are:
///
/// - [`FILE_INFORMATION_CLASS::FileBasicInformation`]
/// - [`FILE_INFORMATION_CLASS::FilePositionInformation`]
///
/// This status will be reflected in the [`HandleContext`], but also through
/// `NtQueryInformationFile` (and therefore, `stat` POSIX operations.)
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
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                return STATUS_ACCESS_VIOLATION;
            }

            // Always zero in my testing.
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
                    handle_context.cursor = (*in_ptr).CurrentByteOffset.QuadPart as _;

                    return STATUS_SUCCESS;
                }
                _ => {}
            }

            // i guess??? thanks windows
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
/// This information will be reflected, for example, during the invocation of the `stat` POSIX API.
/// The path for that event, despite one of the misleading names above, is the
/// [`FILE_INFORMATION_CLASS::FileAllInformation`] match arm.
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
                return STATUS_ACCESS_VIOLATION;
            }

            if !is_memory_valid(io_status_block) {
                return STATUS_ACCESS_VIOLATION;
            }

            match file_information_class {
                // A FILE_BASIC_INFORMATION structure. The caller must have opened the file with
                // the FILE_READ_ATTRIBUTES flag specified in the DesiredAccess parameter.
                FILE_INFORMATION_CLASS::FileBasicInformation => {
                    if handle_context.desired_access & (FILE_READ_ATTRIBUTES) != 0 {
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
                    if (handle_context.desired_access & FILE_READ_DATA != 0)
                        || (handle_context.desired_access & FILE_WRITE_DATA != 0)
                        || (handle_context.desired_access & FILE_READ_ATTRIBUTES != 0)
                    {
                        // Length must be the same size as [`FILE_POSITION_INFORMATION`] length!
                        if length as usize != std::mem::size_of::<FILE_POSITION_INFORMATION>() {
                            return STATUS_ACCESS_VIOLATION;
                        }

                        let out_ptr = file_information as *mut FILE_POSITION_INFORMATION;
                        (*out_ptr).CurrentByteOffset.QuadPart = handle_context.cursor as _;

                        (*io_status_block).Information =
                            std::mem::size_of::<FILE_POSITION_INFORMATION>() as _;
                        (*io_status_block).__bindgen_anon_1.Status =
                            ManuallyDrop::new(STATUS_SUCCESS);
                    }

                    return STATUS_SUCCESS;
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

                    (*out_ptr).AllocationSize.QuadPart =
                        i64::try_from(handle_context.data_xstat_size).unwrap();
                    (*out_ptr).EndOfFile.QuadPart =
                        i64::try_from(handle_context.data_xstat_size).unwrap();
                    (*out_ptr).NumberOfLinks = 0;
                    (*out_ptr).DeletePending = 0;
                    (*out_ptr).Directory = 0;

                    (*io_status_block).Information =
                        std::mem::size_of::<FILE_STANDARD_INFORMATION>() as _;
                    (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);

                    return STATUS_SUCCESS;
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
                    (*out_ptr).AllocationSize.QuadPart =
                        i64::try_from(handle_context.data_xstat_size).unwrap();
                    (*out_ptr).EndOfFile.QuadPart =
                        i64::try_from(handle_context.data_xstat_size).unwrap();
                    (*out_ptr).ReparseTag = 0;
                    (*out_ptr).NumberOfLinks = 0;
                    (*out_ptr).EffectiveAccess = handle_context.desired_access;

                    (*io_status_block).Information =
                        std::mem::size_of::<FILE_STAT_INFORMATION>() as _;
                    (*io_status_block).__bindgen_anon_1.Status = ManuallyDrop::new(STATUS_SUCCESS);

                    return STATUS_SUCCESS;
                }
                // NOTE(gabriela): well, this works as far as `GetFileInformationByHandle` is
                // concerned, at least.
                FILE_INFORMATION_CLASS::FileAllInformation => {
                    // Length must be the same size as [`FILE_ALL_INFORMATION`] length!
                    if length as usize != std::mem::size_of::<FILE_ALL_INFORMATION>() {
                        return STATUS_ACCESS_VIOLATION;
                    }

                    let out_ptr = file_information as *mut FILE_ALL_INFORMATION;

                    fn query_single<T>(handle: HANDLE, kind: FILE_INFORMATION_CLASS) -> Option<T> {
                        let mut single: T = unsafe { MaybeUninit::zeroed().assume_init() };
                        let mut io_status_block: IO_STATUS_BLOCK =
                            unsafe { MaybeUninit::zeroed().assume_init() };

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
                        let mut all_info: FILE_ALL_INFORMATION =
                            unsafe { MaybeUninit::zeroed().assume_init() };

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
                    }
                }
                _ => {}
            }

            // i guess??? thanks windows
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

unsafe extern "system" fn nt_query_attributes_file_hook(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe {
        // NOTE(gabriela): hasn't been needed yet, should be easy to implement! need to create
        // handle and query!

        let original = NT_QUERY_ATTRIBUTES_FILE_ORIGINAL.get().unwrap();
        original(object_attributes, file_basic_info)
    }
}
unsafe extern "system" fn nt_query_volume_information_file_hook(
    file: HANDLE,
    io_status_block: *mut c_void,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(_managed_handle) = handles.get(&file)
        {
            // NOTE(gabriela): stub
            return STATUS_SUCCESS;
        }

        let original = NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            file_information_class,
        )
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
                return STATUS_UNEXPECTED_NETWORK_ERROR;
            }

            return STATUS_SUCCESS;
        }

        let original = NT_CLOSE_ORIGINAL.get().unwrap();
        original(handle)
    }
}

unsafe extern "system" fn get_file_type_hook(handle: HANDLE) -> DWORD {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && handles.contains_key(&handle)
        {
            // TODO(gabriela): come back to this again, remove this hook
            return FILE_TYPE_DISK;
        }

        let original = GET_FILE_TYPE_ORIGINAL.get().unwrap();
        original(handle)
    }
}

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    // ----------------------------------------------------------------------------
    // TODO(gabriela): get rid of kernelbase hooks unless absolutely necessary, try
    // to keep stuff NT-level
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
        "NtWriteFile",
        nt_write_file_hook,
        NtWriteFileType,
        NT_WRITE_FILE_ORIGINAL
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
        "NtQueryAttributesFile",
        nt_query_attributes_file_hook,
        NtQueryAttributesFileType,
        NT_QUERY_ATTRIBUTES_FILE_ORIGINAL
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
        "NtQueryInformationFile",
        nt_query_information_file_hook,
        NtQueryInformationFileType,
        NT_QUERY_INFORMATION_FILE_ORIGINAL
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
        "kernelbase",
        "GetFileType",
        get_file_type_hook,
        GetFileTypeType,
        GET_FILE_TYPE_ORIGINAL
    )?;

    Ok(())
}

//! Module responsible for registering hooks targetting file operations syscalls.

use std::{
    ffi::c_void,
    mem::ManuallyDrop,
    path::PathBuf,
    sync::OnceLock,
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_protocol::file::{
    CloseFileRequest, OpenFileRequest, OpenOptionsInternal, ReadFileRequest,
};
use phnt::ffi::{
    _IO_STATUS_BLOCK, FILE_INFORMATION_CLASS, FS_INFORMATION_CLASS, PFILE_BASIC_INFORMATION,
};
use str_win::u16_buffer_to_string;
use winapi::{
    shared::{
        minwindef::DWORD,
        ntdef::{
            HANDLE, NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID, ULONG,
        },
        ntstatus::{
            STATUS_ACCESS_VIOLATION, STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SUCCESS,
            STATUS_UNEXPECTED_NETWORK_ERROR,
        },
        winerror::ERROR_NO_MORE_ITEMS,
    },
    um::{
        winbase::FILE_TYPE_DISK,
        winnt::{
            ACCESS_MASK, FILE_APPEND_DATA, FILE_WRITE_DATA, GENERIC_WRITE,
        },
    },
};

use crate::{
    apply_hook,
    common::{make_proxy_request_no_response, make_proxy_request_with_response},
    hooks::files::{
        managed_handle::{HandleContext, MANAGED_HANDLES, try_insert_handle},
        util::{WindowsTime, remove_root_dir_from_path, try_xstat},
    },
};

pub mod managed_handle;
pub mod util;

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

        //println!(
        //    "create_file: {name} {file_attributes} {share_access} {create_disposition}
        // {create_options} -> {ret}"
        //);

        // This is usually the case when a Linux path is provided.
        if ret == STATUS_OBJECT_PATH_NOT_FOUND {
            // If no pointer for file handle, return.
            if file_handle.is_null() {
                // TODO(gabriela): implement VirtQuery to check if the vprot and stuff
                // is proper, the Nt* apis do this too.
                return STATUS_ACCESS_VIOLATION;
            }

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
                        // Xstat request contains the size of the remote file in this field,
                        size: xstat.size,
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

                // TODO(gabriela): try to map some common errors to [`NTSTATUS`]-es
                // to facillitate future bug investigations

                return STATUS_OBJECT_PATH_NOT_FOUND;
            }
        }

        ret
    }
}

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
                && let Ok(mut handle_context) = managed_handle.clone().try_write() {
                    if buffer.is_null() {
                        // TODO(gabriela): VirtQuery buffer
                        return STATUS_ACCESS_VIOLATION;
                    }

                    // If we ever succesfully receive the request response, the buffer
                    // we receive can never be bigger than `length`.
                    let req = make_proxy_request_with_response(ReadFileRequest {
                        remote_fd: handle_context.fd,
                        buffer_size: length as _,
                    });

                    if req.is_err() {
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }

                    let req = req.unwrap();

                    match req {
                        Ok(res) => {
                            // Update last access time
                            handle_context.access_time = WindowsTime::current().as_file_time();

                            // If the response's buffer is finally 0, then we finished reading.
                            if res.bytes.is_empty() {
                                (*io_status_block).__bindgen_anon_1.Status =
                                    ManuallyDrop::new(STATUS_SUCCESS);
                                (*io_status_block).Information = 0;

                                return STATUS_SUCCESS;
                            }

                            //let mut read_from_start = false;
                            //// `byte_offset`, if NULL, should be current file offset.
                            //// there's also the scenario of FILE_USE_FILE_POINTER_POSITION for
                            //// LowPart and -1 for HighPart.
                            //// https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntwritefile
                            //// https://github.com/reactos/reactos/blob/0b18dfe92434b030515e0836a5390d8d463915c5/ntoskrnl/io/iomgr/iofunc.c#L2870-L2877
                            //if byte_offset.is_null()
                            //    || ((*byte_offset).u().LowPart == FILE_USE_FILE_POINTER_POSITION
                            //        && (*byte_offset).u().HighPart == -1)
                            //{
                            //    read_from_start = true;
                            //}

                            // Calculate target length for copy.
                            // TODO(gabriela): make sure the contiguous memory pages are all fine
                            // using VirtQuery, otherwise return
                            // STATUS_ACCCESS_VIOLATION.
                            let len = usize::min(res.bytes.len(), length as _);

                            // Otherwise, copy the remote received buffer (relative to
                            // fd cursor) to the provided buffer, as far as we can.
                            std::ptr::copy(res.bytes.as_ptr(), buffer as _, len);

                            // We get this information over IRP normally, so need to replicate the
                            // structure in usermode
                            (*io_status_block).__bindgen_anon_1.Status =
                                ManuallyDrop::new(STATUS_SUCCESS);
                            (*io_status_block).Information = len as _;

                            // Update cursor
                            if byte_offset.is_null() {
                                handle_context.cursor += len;
                            }

                            // Finished reading?
                            if res.bytes.len() < length as _ {
                                return STATUS_SUCCESS;
                            }

                            // There's still some to read, and Windows returns this.
                            return ERROR_NO_MORE_ITEMS as NTSTATUS;
                        }
                        Err(e) => {
                            eprintln!("{:?}", e);
                            // TODO(gabriela): come up with an adequate return.
                            return -1;
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

type NtSetInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut c_void,
    PVOID,
    ULONG,
    FILE_INFORMATION_CLASS,
) -> NTSTATUS;
static NT_SET_INFORMATION_FILE_TYPE_ORIGINAL: OnceLock<&NtSetInformationFileType> = OnceLock::new();

unsafe extern "system" fn nt_set_information_file_hook(
    file: HANDLE,
    io_status_block: *mut c_void,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file) 
                && let Ok(handle_context) = managed_handle.clone().try_write() {
                // The possible values are documented at the following link:
                // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntqueryinformationfile
                //match file_information_class {
                //    // A FILE_BASIC_INFORMATION structure. The caller must have opened the file
                // with the FILE_READ_ATTRIBUTES flag specified in the DesiredAccess parameter.
                //    FILE_INFORMATION_CLASS::FileBasicInformation => {
                //        if handle_context.desired_access & (FILE_READ_ATTRIBUTES) != 0 {
                //            let out_ptr = file_information as *mut FILE_BASIC_INFORMATION;
                //
                //        }

                //    },
                //    _ => todo!()
                //}
                return STATUS_SUCCESS;
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

type NtQueryAttributesFileType =
    unsafe extern "system" fn(POBJECT_ATTRIBUTES, PFILE_BASIC_INFORMATION) -> NTSTATUS;
static NT_QUERY_ATTRIBUTES_FILE_ORIGINAL: OnceLock<&NtQueryAttributesFileType> = OnceLock::new();

unsafe extern "system" fn nt_query_attributes_file_hook(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe {
        let name = read_object_attributes_name(object_attributes);
        println!("query_attributes: {name}");

        let original = NT_QUERY_ATTRIBUTES_FILE_ORIGINAL.get().unwrap();
        original(object_attributes, file_basic_info)
    }
}
type NtQueryVolumeInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut c_void, PVOID, ULONG, FS_INFORMATION_CLASS) -> NTSTATUS;
static NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryVolumeInformationFileType> =
    OnceLock::new();

unsafe extern "system" fn nt_query_volume_information_file_hook(
    file: HANDLE,
    io_status_block: *mut c_void,
    fs_information: PVOID,
    length: ULONG,
    file_information_class: FS_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file) {
                return STATUS_SUCCESS;
        }
        let original = NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            fs_information,
            length,
            file_information_class,
        )
    }
}

type NtQueryInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut c_void, PVOID, ULONG, FS_INFORMATION_CLASS) -> NTSTATUS;
static NT_QUERY_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryInformationFileType> = OnceLock::new();

unsafe extern "system" fn nt_query_information_file_hook(
    file: HANDLE,
    io_status_block: *mut c_void,
    fs_information: PVOID,
    length: ULONG,
    file_information_class: FS_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && let Some(managed_handle) = handles.get(&file) {
                return STATUS_SUCCESS;
        }

        let original = NT_QUERY_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            fs_information,
            length,
            file_information_class,
        )
    }
}

type NtCloseType = unsafe extern "system" fn(HANDLE) -> NTSTATUS;
static NT_CLOSE_ORIGINAL: OnceLock<&NtCloseType> = OnceLock::new();

unsafe extern "system" fn nt_close_hook(handle: HANDLE) -> NTSTATUS {
    unsafe {
        if let Ok(mut handles) = MANAGED_HANDLES.try_write()
            && let Some(managed_handle) = handles.get(&handle)
                && let Ok(handle_context) = managed_handle.clone().try_read() {
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

type GetFileTypeType = unsafe extern "system" fn(HANDLE) -> DWORD;
static GET_FILE_TYPE_ORIGINAL: OnceLock<&GetFileTypeType> = OnceLock::new();

unsafe extern "system" fn get_file_type_hook(handle: HANDLE) -> DWORD {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read()
            && handles.contains_key(&handle) {
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

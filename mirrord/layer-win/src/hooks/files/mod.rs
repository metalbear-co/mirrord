//! Module responsible for registering hooks targetting file operations syscalls.

use std::{
    ffi::c_void,
    mem::MaybeUninit,
    path::PathBuf,
    sync::OnceLock,
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_protocol::file::{
    OpenFileRequest, OpenOptionsInternal, ReadFileRequest,
};
use phnt::ffi::{FILE_INFORMATION_CLASS, FS_INFORMATION_CLASS, PFILE_BASIC_INFORMATION};
use str_win::u16_buffer_to_string;
use winapi::{
    shared::{
        minwindef::{DWORD, FILETIME},
        ntdef::{
            HANDLE, NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID, ULONG,
        },
        ntstatus::{STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SUCCESS},
    },
    um::{
        fileapi::BY_HANDLE_FILE_INFORMATION,
        minwinbase::SYSTEMTIME,
        sysinfoapi::GetSystemTime,
        timezoneapi::SystemTimeToFileTime,
        winbase::FILE_TYPE_DISK,
        winnt::{ACCESS_MASK, FILE_APPEND_DATA, FILE_ATTRIBUTE_READONLY, FILE_WRITE_DATA},
    },
};

use crate::{
    apply_hook,
    common::make_proxy_request_with_response,
    hooks::files::{
        managed_handle::{HandleContext, MANAGED_HANDLES, try_insert_handle},
        util::remove_root_dir_from_path,
    },
};

pub mod managed_handle;
pub mod util;

fn read_object_attributes_name(object_attributes: POBJECT_ATTRIBUTES) -> String {
    unsafe {
        let name_ustr = (*object_attributes).ObjectName;
        let name_raw =
            &*std::ptr::slice_from_raw_parts((*name_ustr).Buffer, (*name_ustr).Length as _);
        u16_buffer_to_string(name_raw)
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
            // Try to get path.
            let linux_path = remove_root_dir_from_path(name);

            // If no path, return.
            if linux_path.is_none() {
                return ret;
            }
            let linux_path = linux_path.unwrap();
            if desired_access & (FILE_WRITE_DATA | FILE_APPEND_DATA) != 0 {
                println!("WARNING: currently unsupported creating file for r/w");
                return ret;
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
                return ret;
            }
            let req = req.unwrap();

            let managed_handle = match req {
                Ok(file) => {
                    let mut system_time: SYSTEMTIME = MaybeUninit::zeroed().assume_init();
                    GetSystemTime(&mut system_time as _);
                    let mut creation_time: FILETIME = MaybeUninit::zeroed().assume_init();
                    SystemTimeToFileTime(&system_time as _, &mut creation_time as _);

                    try_insert_handle(HandleContext {
                        fd: file.fd,
                        desired_access,
                        file_attributes,
                        share_access,
                        create_disposition,
                        create_options,
                        path: linux_path,
                        creation_time,
                    })
                }
                Err(e) => {
                    eprintln!("{:?} {}", e, linux_path);
                    None
                }
            };

            if managed_handle.is_some() && !file_handle.is_null() {
                // Write managed handle at the provided pointer
                *file_handle = managed_handle.unwrap();
                println!(
                    "Succesfully wrote managed handle! {:8p}",
                    managed_handle.unwrap()
                );
                return STATUS_SUCCESS;
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
    *mut c_void,
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
    io_status_block: *mut c_void,
    buffer: PVOID,
    length: ULONG,
    byte_offset: PLARGE_INTEGER,
    key: PULONG,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read() {
            if let Some(handle_context) = handles.get(&file) {
                if buffer.is_null() {
                    return -1;
                }

                if length == 0 {
                    return -1;
                }

                let req = make_proxy_request_with_response(ReadFileRequest {
                    remote_fd: handle_context.fd,
                    buffer_size: length as _,
                });

                if req.is_err() {
                    return -1;
                }

                let req = req.unwrap();

                match req {
                    Ok(res) => {
                        std::ptr::copy(
                            res.bytes.as_ptr(),
                            *(buffer as *mut *mut _) as _,
                            res.bytes.len(),
                        );
                        return STATUS_SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("{:?}", e);
                        return -1;
                    }
                }
            }
        }

        let original = NT_READ_FILE_ORIGINAL.get().unwrap();
        let ret = original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            buffer,
            length,
            byte_offset,
            key,
        );

        println!("read_file_hook: buf:{:8p} len:{} -> {ret}", buffer, length);

        ret
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
    file_infromation: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read() {
            if handles.contains_key(&file) {
                // TODO
                return STATUS_SUCCESS;
            }
        }

        let original = NT_SET_INFORMATION_FILE_TYPE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_infromation,
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
        if let Ok(handles) = MANAGED_HANDLES.try_read() {
            // TODO(gabriela): this checks the info for **file**, not **folder**!!! Please fix.
            if let Some(handle_context) = handles.get(&file) {
                let mut info: BY_HANDLE_FILE_INFORMATION = MaybeUninit::zeroed().assume_init();
                info.dwFileAttributes = FILE_ATTRIBUTE_READONLY;

                // TODO(gabriela): explain, fix return
                if length != 0x18 {
                    return -1;
                }

                info.ftCreationTime = handle_context.creation_time.clone();
                info.ftLastAccessTime = handle_context.creation_time.clone();
                info.ftLastWriteTime = handle_context.creation_time.clone();

                return STATUS_SUCCESS;
            }
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
        if let Ok(handles) = MANAGED_HANDLES.try_read() {
            if let Some(handle_context) = handles.get(&file) {
                let mut info: BY_HANDLE_FILE_INFORMATION = MaybeUninit::zeroed().assume_init();
                info.dwFileAttributes = FILE_ATTRIBUTE_READONLY;

                // TODO(gabriela): explain, improve return
                if length != 0x68 {
                    return -1;
                }

                // TODO(gabriela): this is wrong, fix! fstat?
                info.ftCreationTime = handle_context.creation_time.clone();
                info.ftLastAccessTime = handle_context.creation_time.clone();
                info.ftLastWriteTime = handle_context.creation_time.clone();

                // TODO(gabriela): I know nothing about NTFS and don't wish to either
                // but I should figure this out
                info.nNumberOfLinks = 0;
                info.nFileIndexLow = 0;
                info.nFileIndexLow = 1;

                // TODO(gabriela): get actual bounds!
                info.nFileSizeLow = 1000;
                info.nFileSizeHigh = 0;

                // TODO(gabriela): this is just wrong but python doesn't complain
                // anyway what the hell do we put here? idk
                info.dwVolumeSerialNumber = 1;

                return STATUS_SUCCESS;
            }
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
        if let Ok(mut handles) = MANAGED_HANDLES.try_write() {
            if handles.contains_key(&handle) {
                handles.remove_entry(&handle);
                return STATUS_SUCCESS;
            }
        }

        let original = NT_CLOSE_ORIGINAL.get().unwrap();
        original(handle)
    }
}

type GetFileTypeType = unsafe extern "system" fn(HANDLE) -> DWORD;
static GET_FILE_TYPE_ORIGINAL: OnceLock<&GetFileTypeType> = OnceLock::new();

unsafe extern "system" fn get_file_type_hook(handle: HANDLE) -> DWORD {
    unsafe {
        if let Ok(handles) = MANAGED_HANDLES.try_read() {
            if handles.contains_key(&handle) {
                // TODO(gabriela): come back to this again, get rid of kernelbase hooks
                return FILE_TYPE_DISK;
            }
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

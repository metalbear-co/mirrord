//! Module responsible for registering hooks targetting file operations syscalls.

use core::slice::SlicePattern;
use std::{collections::HashMap, cell::LazyCell, ffi::c_void, path::PathBuf, sync::{OnceLock, RwLock}};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_protocol::{file::{OpenFileRequest, OpenOptionsInternal, ReadFileRequest}, FileRequest};
use str_win::u16_buffer_to_string;
use winapi::{
    shared::{minwindef::DWORD, ntdef::{
        HANDLE, NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID, ULONG,
    }, ntstatus::{STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SUCCESS}},
    um::{winbase::FILE_TYPE_DISK, winnt::{ACCESS_MASK, FILE_APPEND_DATA, FILE_READ_DATA, FILE_WRITE_DATA}},
};

use crate::{apply_hook, common::make_proxy_request_with_response, hooks::files::{managed_handle::{try_insert_handle, HandleContext, MANAGED_HANDLES}, util::{remove_root_dir_from_path}}, PROXY_CONNECTION};

pub mod util;
pub mod managed_handle;

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

        let name_ustr = (*object_attributes).ObjectName;
        let name_raw =
            &*std::ptr::slice_from_raw_parts((*name_ustr).Buffer, (*name_ustr).Length as _);
        let name = u16_buffer_to_string(name_raw);

        //println!(
        //    "create_file: {name} {file_attributes} {share_access} {create_disposition} {create_options} -> {ret}"
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

            // Make sure we're only trying to access as read.
            //if desired_access & (FILE_WRITE_DATA | FILE_APPEND_DATA) != 0 {
            //    println!("WARNING: currently unsupported creating file for r/w");
            //    return ret;
            //}

            // Get open options.
            let open_options = OpenOptionsInternal {
                read: true,
                ..Default::default()
            };

            // Try to open the file on the pod.
            let req = make_proxy_request_with_response(OpenFileRequest {
                path: PathBuf::from(&linux_path),
                open_options
            });
            
            // Try to get request.
            if req.is_err() {
                return ret;
            }
            let req = req.unwrap();

            let managed_handle = match req {
                Ok(file) => {
                    try_insert_handle(HandleContext {
                        fd: file.fd,
                        desired_access,
                        file_attributes,
                        share_access,
                        create_disposition,
                        create_options,
                        path: linux_path
                    })
                },
                Err(e) => {
                    eprintln!("{:?} {}", e, linux_path);
                    None
                }
            };
        
            if managed_handle.is_some() && !file_handle.is_null() {
                // Write managed handle at the provided pointer
                *file_handle = managed_handle.unwrap();
                println!("Succesfully wrote managed handle! {:8p}", managed_handle.unwrap());
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
                    buffer_size: length as _
                });

                if req.is_err() {
                    return -1;
                }

                let req = req.unwrap();

                match req {
                    Ok(res) => {
                        std::ptr::copy(res.bytes.as_ptr(), *(buffer as *mut *mut _) as _, res.bytes.len());
                        return STATUS_SUCCESS;
                    },
                    Err(e) => {
                       eprintln!("{:?}", e) ;
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
                return FILE_TYPE_DISK;
            }
        }

        let original = GET_FILE_TYPE_ORIGINAL.get().unwrap();
        original(handle)
    }
}


pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
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

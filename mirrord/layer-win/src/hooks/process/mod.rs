//! Module responsible for registering hooks targetting process creation syscalls.

use std::{ffi::c_void, sync::OnceLock};

use minhook_detours_rs::guard::DetourGuard;
use winapi::{
    shared::ntdef::{BOOLEAN, NTSTATUS, PCOBJECT_ATTRIBUTES, PHANDLE, ULONG},
    um::winnt::{ACCESS_MASK, HANDLE},
};

use crate::apply_hook;

// https://github.com/winsiderss/systeminformer/blob/f9c238893e0b1c8c82c2e4a3c8d26e871c8f09fe/phnt/include/ntpsapi.h#L1890
type NtCreateProcessType = unsafe extern "system" fn(
    PHANDLE,
    ACCESS_MASK,
    PCOBJECT_ATTRIBUTES,
    HANDLE,
    BOOLEAN,
    HANDLE,
    HANDLE,
    HANDLE,
) -> NTSTATUS;
static NT_CREATE_PROCESS_ORIGINAL: OnceLock<&NtCreateProcessType> = OnceLock::new();

unsafe extern "system" fn nt_create_process_hook(
    process_handle_ptr: PHANDLE,
    desired_access: ACCESS_MASK,
    object_attributes_ptr: PCOBJECT_ATTRIBUTES,
    parent_proess: HANDLE,
    inherit_object_table: BOOLEAN,
    section_handle: HANDLE,
    debug_port: HANDLE,
    token_handle: HANDLE,
) -> NTSTATUS {
    unsafe {
        let original = NT_CREATE_PROCESS_ORIGINAL.get().unwrap();
        original(
            process_handle_ptr,
            desired_access,
            object_attributes_ptr,
            parent_proess,
            inherit_object_table,
            section_handle,
            debug_port,
            token_handle,
        )
    }
}

// https://github.com/winsiderss/systeminformer/blob/f9c238893e0b1c8c82c2e4a3c8d26e871c8f09fe/phnt/include/ntpsapi.h#L1928
type NtCreateProcessExType = unsafe extern "system" fn(
    PHANDLE,
    ACCESS_MASK,
    PCOBJECT_ATTRIBUTES,
    HANDLE,
    ULONG,
    HANDLE,
    HANDLE,
    HANDLE,
    ULONG,
) -> NTSTATUS;
static NT_CREATE_PROCESS_EX_ORIGINAL: OnceLock<&NtCreateProcessExType> = OnceLock::new();

unsafe extern "system" fn nt_create_process_ex_hook(
    process_handle_ptr: PHANDLE,
    desired_access: ACCESS_MASK,
    object_attributes_ptr: PCOBJECT_ATTRIBUTES,
    parent_proess: HANDLE,
    flags: ULONG,
    section_handle: HANDLE,
    debug_port: HANDLE,
    token_handle: HANDLE,
    reserved: ULONG,
) -> NTSTATUS {
    unsafe {
        let original = NT_CREATE_PROCESS_EX_ORIGINAL.get().unwrap();
        original(
            process_handle_ptr,
            desired_access,
            object_attributes_ptr,
            parent_proess,
            flags,
            section_handle,
            debug_port,
            token_handle,
            reserved,
        )
    }
}

// https://github.com/winsiderss/systeminformer/blob/f9c238893e0b1c8c82c2e4a3c8d26e871c8f09fe/phnt/include/ntpsapi.h#L3284
type NtCreateUserProcessType = unsafe extern "system" fn(
    PHANDLE,
    PHANDLE,
    ACCESS_MASK,
    ACCESS_MASK,
    PCOBJECT_ATTRIBUTES,
    PCOBJECT_ATTRIBUTES,
    ULONG,
    ULONG,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> NTSTATUS;
static NT_CREATE_USER_PROCESS_ORIGINAL: OnceLock<&NtCreateUserProcessType> = OnceLock::new();

unsafe extern "system" fn nt_create_user_process_hook(
    process_handle_ptr: PHANDLE,
    thread_handle_ptr: PHANDLE,
    process_desired_access: ACCESS_MASK,
    thread_desired_access: ACCESS_MASK,
    process_object_attributes: PCOBJECT_ATTRIBUTES,
    thread_object_attributes: PCOBJECT_ATTRIBUTES,
    process_flags: ULONG,
    thread_flags: ULONG,
    unk1: *mut c_void,
    unk2: *mut c_void,
    unk3: *mut c_void,
) -> NTSTATUS {
    unsafe {
        // TODO:
        // - get current DLL handle
        // - get full path to handle
        // - make process start suspended
        // - inject dll into process
        // - resume process

        let original = NT_CREATE_USER_PROCESS_ORIGINAL.get().unwrap();
        original(
            process_handle_ptr,
            thread_handle_ptr,
            process_desired_access,
            thread_desired_access,
            process_object_attributes,
            thread_object_attributes,
            process_flags,
            thread_flags,
            unk1,
            unk2,
            unk3,
        )
    }
}

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    apply_hook!(
        guard,
        "ntdll",
        "NtCreateProcess",
        nt_create_process_hook,
        NtCreateProcessType,
        NT_CREATE_PROCESS_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtCreateProcessEx",
        nt_create_process_ex_hook,
        NtCreateProcessExType,
        NT_CREATE_PROCESS_EX_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ntdll",
        "NtCreateUserProcess",
        nt_create_user_process_hook,
        NtCreateUserProcessType,
        NT_CREATE_USER_PROCESS_ORIGINAL
    )?;

    Ok(())
}

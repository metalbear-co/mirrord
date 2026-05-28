//! NT function-pointer type aliases for every file syscall we hook +
//! the `OnceLock<&Original>` pointers populated by `apply_hook!` at
//! init time. Lifted out of `mod.rs` so the actual hook bodies and
//! dispatch can read top-to-bottom without scrolling past ~150 lines
//! of FFI signatures.
//!
//! Adding a new hook is a three-line affair:
//! 1. Add the type alias here (next to its peers).
//! 2. Add the matching `OnceLock<&NtXxxType>` static.
//! 3. Add an `apply_hook!` call in `super::initialize_hooks`.

use std::{ffi::c_void, sync::OnceLock};

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

// https://github.com/winsiderss/phnt/blob/fc1f96ee976635f51faa89896d1d805eb0586350/ntioapi.h#L1611
pub(super) type NtCreateFileType = unsafe extern "system" fn(
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
pub(super) static NT_CREATE_FILE_ORIGINAL: OnceLock<&NtCreateFileType> = OnceLock::new();

// https://github.com/winsiderss/phnt/blob/fc1f96ee976635f51faa89896d1d805eb0586350/ntioapi.h#L1963
pub(super) type NtReadFileType = unsafe extern "system" fn(
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
pub(super) static NT_READ_FILE_ORIGINAL: OnceLock<&NtReadFileType> = OnceLock::new();

// https://github.com/winsiderss/phnt/blob/fc1f96ee976635f51faa89896d1d805eb0586350/ntioapi.h#L1978
pub(super) type NtWriteFileType = unsafe extern "system" fn(
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
pub(super) static NT_WRITE_FILE_ORIGINAL: OnceLock<&NtWriteFileType> = OnceLock::new();

pub(super) type NtSetInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PVOID,
    ULONG,
    FILE_INFORMATION_CLASS,
) -> NTSTATUS;
pub(super) static NT_SET_INFORMATION_FILE_ORIGINAL: OnceLock<&NtSetInformationFileType> =
    OnceLock::new();

pub(super) type NtSetVolumeInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK, PVOID, ULONG, FSINFOCLASS) -> NTSTATUS;
pub(super) static NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL: OnceLock<
    &NtSetVolumeInformationFileType,
> = OnceLock::new();

pub(super) type NtSetQuotaInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK, PVOID, ULONG) -> NTSTATUS;
pub(super) static NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL: OnceLock<&NtSetQuotaInformationFileType> =
    OnceLock::new();

pub(super) type NtQueryInformationFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PVOID,
    ULONG,
    FILE_INFORMATION_CLASS,
) -> NTSTATUS;
pub(super) static NT_QUERY_INFORMATION_FILE_ORIGINAL: OnceLock<&NtQueryInformationFileType> =
    OnceLock::new();

pub(super) type NtQueryAttributesFileType =
    unsafe extern "system" fn(POBJECT_ATTRIBUTES, PFILE_BASIC_INFORMATION) -> NTSTATUS;
pub(super) static NT_QUERY_ATTRIBUTES_FILE_ORIGINAL: OnceLock<&NtQueryAttributesFileType> =
    OnceLock::new();

pub(super) type NtQueryVolumeInformationFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK, PVOID, ULONG, FSINFOCLASS) -> NTSTATUS;
pub(super) static NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL: OnceLock<
    &NtQueryVolumeInformationFileType,
> = OnceLock::new();

pub(super) type NtQueryQuotaInformationFileType = unsafe extern "system" fn(
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
pub(super) static NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL: OnceLock<
    &NtQueryQuotaInformationFileType,
> = OnceLock::new();

pub(super) type NtDeleteFileType = unsafe extern "system" fn(POBJECT_ATTRIBUTES) -> NTSTATUS;
pub(super) static NT_DELETE_FILE_ORIGINAL: OnceLock<&NtDeleteFileType> = OnceLock::new();

pub(super) type NtDeviceIoControlFileType = unsafe extern "system" fn(
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
pub(super) static NT_DEVICE_IO_CONTROL_FILE_ORIGINAL: OnceLock<&NtDeviceIoControlFileType> =
    OnceLock::new();

pub(super) type NtLockFileType = unsafe extern "system" fn(
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
pub(super) static NT_LOCK_FILE_ORIGINAL: OnceLock<&NtLockFileType> = OnceLock::new();

pub(super) type NtUnlockFileType = unsafe extern "system" fn(
    HANDLE,
    *mut _IO_STATUS_BLOCK,
    PLARGE_INTEGER,
    PLARGE_INTEGER,
    ULONG,
) -> NTSTATUS;
pub(super) static NT_UNLOCK_FILE_ORIGINAL: OnceLock<&NtUnlockFileType> = OnceLock::new();

pub(super) type NtCloseType = unsafe extern "system" fn(HANDLE) -> NTSTATUS;
pub(super) static NT_CLOSE_ORIGINAL: OnceLock<&NtCloseType> = OnceLock::new();

pub(super) type NtCancelIoFileType =
    unsafe extern "system" fn(HANDLE, *mut _IO_STATUS_BLOCK) -> NTSTATUS;
pub(super) static NT_CANCEL_IO_FILE_ORIGINAL: OnceLock<&NtCancelIoFileType> = OnceLock::new();

pub(super) type NtWaitForSingleObjectType =
    unsafe extern "system" fn(HANDLE, BOOLEAN, PLARGE_INTEGER) -> NTSTATUS;
pub(super) static NT_WAIT_FOR_SINGLE_OBJECT_ORIGINAL: OnceLock<&NtWaitForSingleObjectType> =
    OnceLock::new();

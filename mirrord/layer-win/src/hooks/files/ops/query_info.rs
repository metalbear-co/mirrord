//! Bodies of `nt_query_information_file_hook` and
//! `nt_query_volume_information_file_hook`. The hooks in `super::super`
//! are thin delegates into [`info`] and [`volume_info`].
//!
//! ## `info` — `nt_query_information_file_hook`
//!
//! Reports a managed file's metadata to the caller. Handled classes:
//!
//! - [`FILE_INFORMATION_CLASS::FileBasicInformation`] — timestamps + attributes from the local
//!   [`HandleContext`](crate::hooks::files::managed_handle::HandleContext).
//! - [`FILE_INFORMATION_CLASS::FilePositionInformation`] — read the agent's remote cursor (the
//!   source of truth for stored position; `try_seek(Current(0))` peeks). Requires
//!   `FILE_READ_ATTRIBUTES`.
//! - [`FILE_INFORMATION_CLASS::FileStandardInformation`] — size + counters from a remote `stat`
//!   (via `try_xstat`).
//! - [`FILE_INFORMATION_CLASS::FileStatInformation`] — same data, NT's newer combined struct (used
//!   by POSIX `stat`).
//! - [`FILE_INFORMATION_CLASS::FileAllInformation`] — composite of the above; emitted via
//!   `query_all` which fans out internally.
//!
//! Any other class returns [`STATUS_INVALID_INFO_CLASS`] with a warning
//! trace. (Earlier the catch-all returned [`STATUS_ACCESS_VIOLATION`],
//! which broke .NET's `FileStream` init by surfacing as
//! `IOException: Invalid access to memory location`.)
//!
//! Invalid `file_information` / `io_status_block` pointers return
//! [`STATUS_ACCESS_VIOLATION`]; agent failures return
//! [`STATUS_UNEXPECTED_NETWORK_ERROR`]; non-managed files fall through
//! to the original syscall.
//!
//! ## `volume_info` — `nt_query_volume_information_file_hook`
//!
//! Queries the volume on which a managed file finds itself. The type
//! of information being queried is distinguished by the [`FSINFOCLASS`]
//! value. Currently supported entries:
//!
//! - [`FSINFOCLASS::FileFsDeviceInformation`] — fakes a virtual disk read-only device so
//!   `kernelbase!GetFileType` reports `FILE_TYPE_DISK`.
//! - [`FSINFOCLASS::FileFsVolumeInformation`] — fakes a `C:` volume with the current time as
//!   creation time; minimal enough for Node.
//!
//! Other classes return [`STATUS_ACCESS_VIOLATION`] (the documented NT
//! response on classes the OS rejects). Non-managed files fall through
//! to the original syscall.

use std::mem::ManuallyDrop;

use mirrord_protocol::file::SeekFromInternal;
use phnt::ffi::{
    _IO_STATUS_BLOCK, _LARGE_INTEGER, FILE_ALL_INFORMATION, FILE_BASIC_INFORMATION,
    FILE_FS_DEVICE_INFORMATION, FILE_FS_VOLUME_INFORMATION, FILE_INFORMATION_CLASS,
    FILE_POSITION_INFORMATION, FILE_READ_ONLY_DEVICE, FILE_STANDARD_INFORMATION, FSINFOCLASS,
};
use winapi::{
    shared::{
        ntdef::{FALSE, HANDLE, NTSTATUS, PVOID, ULONG},
        ntstatus::{
            STATUS_ACCESS_VIOLATION, STATUS_INVALID_INFO_CLASS, STATUS_SUCCESS,
            STATUS_UNEXPECTED_NETWORK_ERROR,
        },
    },
    um::{
        winioctl::FILE_DEVICE_VIRTUAL_DISK,
        winnt::{ACCESS_MASK, FILE_READ_ATTRIBUTES},
    },
};

use crate::hooks::files::{
    iosb::{check_io_pointers, write_iosb_success},
    managed_handle::MANAGED_HANDLES,
    types::{NT_QUERY_INFORMATION_FILE_ORIGINAL, NT_QUERY_VOLUME_INFORMATION_FILE_ORIGINAL},
    util::{WindowsTime, try_seek, try_xstat},
};

/// Body of `nt_query_information_file_hook`.
pub(in crate::hooks::files) unsafe fn info(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            if let Err(status) = check_io_pointers(file_information, io_status_block) {
                tracing::warn!(
                    handle = ?file,
                    fd = handle_context.fd,
                    path = handle_context.path,
                    status_code = format!("{status:#x}"),
                    "nt_query_information_file_hook: invalid IO pointer pre-flight, returning status"
                );
                return status;
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

                        write_iosb_success(
                            io_status_block,
                            std::mem::size_of::<FILE_BASIC_INFORMATION>(),
                        );
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

                            write_iosb_success(
                                io_status_block,
                                std::mem::size_of::<FILE_POSITION_INFORMATION>(),
                            );

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

                        write_iosb_success(
                            io_status_block,
                            std::mem::size_of::<FILE_STANDARD_INFORMATION>(),
                        );

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

                        write_iosb_success(
                            io_status_block,
                            std::mem::size_of::<FILE_STAT_INFORMATION>(),
                        );

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
                            info(
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

                        write_iosb_success(
                            io_status_block,
                            std::mem::size_of::<FILE_ALL_INFORMATION>(),
                        );

                        return STATUS_SUCCESS;
                    } else {
                        tracing::error!(
                            "nt_query_information_file_hook: Failed querying all information because one of the single queries failed!"
                        );
                        return STATUS_UNEXPECTED_NETWORK_ERROR;
                    }
                }
                _ => {
                    // Same pragmatic fallback as
                    // `nt_set_information_file_hook`'s catch-all.
                    //
                    // Returning `STATUS_ACCESS_VIOLATION` here trips
                    // .NET's `FileStream` init into:
                    //
                    //   "IOException: Invalid access to memory location"
                    //
                    // on every async open.
                    //
                    // For metadata we don't model, report
                    // `STATUS_INVALID_INFO_CLASS` so the caller can
                    // decide whether the missing data matters. Most
                    // clients fall back to defaults; .NET in particular
                    // tolerates this for the classes it queries
                    // opportunistically.
                    tracing::warn!(
                        path = handle_context.path,
                        "nt_query_information_file_hook: file_information_class: {:?} not implemented; reporting STATUS_INVALID_INFO_CLASS",
                        file_information_class
                    );
                    return STATUS_INVALID_INFO_CLASS;
                }
            }
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

/// Body of `nt_query_volume_information_file_hook`.
pub(in crate::hooks::files) unsafe fn volume_info(
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
            if let Err(status) = check_io_pointers(file_information, io_status_block) {
                tracing::warn!(
                    handle = ?file,
                    fd = handle_context.fd,
                    path = handle_context.path,
                    status_code = format!("{status:#x}"),
                    "nt_query_volume_information_file_hook: invalid IO pointer pre-flight, returning status"
                );
                return status;
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

                    write_iosb_success(
                        io_status_block,
                        std::mem::size_of::<FILE_FS_DEVICE_INFORMATION>(),
                    );

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

                    write_iosb_success(
                        io_status_block,
                        std::mem::size_of::<FILE_FS_VOLUME_INFORMATION>(),
                    );

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

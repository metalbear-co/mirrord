//! Body of `nt_set_information_file_hook`. The hook in `super::super`
//! is a thin delegate into [`handle`].
//!
//! [`handle`] modifies a managed file's local state
//! ([`HandleContext`](crate::hooks::files::managed_handle::HandleContext)),
//! remote agent state (file cursor, attributes), or IOCP binding,
//! depending on the `file_information_class`.
//!
//! ## Handled classes
//!
//! - [`FILE_INFORMATION_CLASS::FileBasicInformation`] — update timestamps + attributes locally;
//!   requires `FILE_WRITE_ATTRIBUTES` access.
//! - [`FILE_INFORMATION_CLASS::FilePositionInformation`] — seek the agent's remote fd to the new
//!   offset; the layer treats the agent cursor as the source of truth for the stored position.
//!   Query via `NtQueryInformationFile(FilePositionInformation)` reads it back. Note: this is a
//!   no-op on async handles, since async reads always carry an explicit `ByteOffset` and don't
//!   consult the stored position.
//! - [`FILE_INFORMATION_CLASS::FileCompletionInformation`] — bind the file to an OS IOCP via
//!   [`HandleContext::bind_iocp`](crate::hooks::files::managed_handle::HandleContext::bind_iocp).
//!   The port itself is a real OS handle owned end-to-end by the OS; the layer just records the
//!   `(port, key)` on the file's `HandleContext` so the async-read worker knows where to post
//!   completion packets.
//! - [`FILE_INFORMATION_CLASS::FileReplaceCompletionInformation`] (Win 8.1+) — replace/remove the
//!   binding; `Port == NULL` is the documented "unbind" path.
//! - [`FILE_INFORMATION_CLASS::FileIoCompletionNotificationInformation`] — parse
//!   `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` and store on the `HandleContext`. The async-read fast
//!   path consults this flag to decide whether to skip the completion packet on inline success.
//!
//! Any other class returns [`STATUS_SUCCESS`] with a warning trace --
//! pretending we honored the set lets clients like .NET's `FileStream`
//! init (which sets non-essential classes on every open) proceed
//! without surfacing `IOException: Invalid access to memory location`
//! from our former [`STATUS_ACCESS_VIOLATION`] fallback.
//!
//! ## Errors
//!
//! Invalid `file_information` / `io_status_block` pointers return
//! [`STATUS_ACCESS_VIOLATION`]; agent failures return
//! [`STATUS_UNEXPECTED_NETWORK_ERROR`]; the file-not-managed case
//! falls through to the original syscall.

use mirrord_protocol::file::SeekFromInternal;
use phnt::ffi::{
    _IO_STATUS_BLOCK, _LARGE_INTEGER, FILE_BASIC_INFORMATION, FILE_COMPLETION_INFORMATION,
    FILE_INFORMATION_CLASS, FILE_IO_COMPLETION_NOTIFICATION_INFORMATION, FILE_POSITION_INFORMATION,
};
use winapi::{
    shared::{
        minwindef::FILETIME,
        ntdef::{HANDLE, NTSTATUS, PVOID, ULONG},
        ntstatus::{
            STATUS_ACCESS_VIOLATION, STATUS_INFO_LENGTH_MISMATCH, STATUS_SUCCESS,
            STATUS_UNEXPECTED_NETWORK_ERROR,
        },
    },
    um::{winbase::FILE_SKIP_COMPLETION_PORT_ON_SUCCESS, winnt::FILE_WRITE_ATTRIBUTES},
};

use crate::hooks::files::{
    iosb::{check_io_pointers, write_iosb_success},
    managed_handle::MANAGED_HANDLES,
    types::NT_SET_INFORMATION_FILE_ORIGINAL,
    util::try_seek,
};

/// Body of `nt_set_information_file_hook`.
pub(in crate::hooks::files) unsafe fn handle(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    file_information_class: FILE_INFORMATION_CLASS,
) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&file)
            && let Ok(mut handle_context) = managed_handle.try_write()
        {
            if let Err(status) = check_io_pointers(file_information, io_status_block) {
                tracing::warn!(
                    handle = ?file,
                    fd = handle_context.fd,
                    path = handle_context.path,
                    status_code = format!("{status:#x}"),
                    "nt_set_information_file_hook: invalid IO pointer pre-flight, returning status"
                );
                return status;
            }

            // NOTE(gabriela): In testing, this has always been the IoStatusBlock result.
            write_iosb_success(io_status_block, 0);

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
                // Bind the managed file to an IO completion port. The
                // port is a real OS handle the caller obtained from
                // `NtCreateIoCompletion`; we just record the (port, key)
                // on the file's `HandleContext` so the async-read worker
                // knows where to post the completion packet.
                //
                // The "file already bound" case (spec §3,
                // `Test_DoubleBindRejected`) is rejected with
                // `STATUS_INVALID_PARAMETER` inside
                // `HandleContext::bind_iocp` -- this arm just forwards
                // whatever status it returns.
                FILE_INFORMATION_CLASS::FileCompletionInformation => {
                    if (length as usize) < std::mem::size_of::<FILE_COMPLETION_INFORMATION>() {
                        return STATUS_INFO_LENGTH_MISMATCH;
                    }
                    let info = &*(file_information as *const FILE_COMPLETION_INFORMATION);
                    return handle_context.bind_iocp(info.Port as _, info.Key as usize);
                }
                // Replace or remove the binding (Windows 8.1+). Port =
                // NULL is the documented "unbind" path, anything else
                // re-binds (we unconditionally unbind first so the
                // bind-while-bound rejection from FileCompletionInformation
                // doesn't fire).
                FILE_INFORMATION_CLASS::FileReplaceCompletionInformation => {
                    if (length as usize) < std::mem::size_of::<FILE_COMPLETION_INFORMATION>() {
                        return STATUS_INFO_LENGTH_MISMATCH;
                    }
                    let info = &*(file_information as *const FILE_COMPLETION_INFORMATION);
                    handle_context.unbind_iocp();
                    if info.Port.is_null() {
                        return STATUS_SUCCESS;
                    }
                    return handle_context.bind_iocp(info.Port as _, info.Key as usize);
                }
                // FILE_SKIP_COMPLETION_PORT_ON_SUCCESS and friends. We
                // only track the skip-on-success bit; the other flags
                // (skip-set-event-on-handle, skip-set-user-event-on-fast-io)
                // don't matter for our model since we never use those
                // signaling paths.
                FILE_INFORMATION_CLASS::FileIoCompletionNotificationInformation => {
                    if (length as usize)
                        < std::mem::size_of::<FILE_IO_COMPLETION_NOTIFICATION_INFORMATION>()
                    {
                        return STATUS_INFO_LENGTH_MISMATCH;
                    }
                    let info =
                        &*(file_information as *const FILE_IO_COMPLETION_NOTIFICATION_INFORMATION);
                    handle_context.skip_on_success =
                        (info.Flags & FILE_SKIP_COMPLETION_PORT_ON_SUCCESS as u32) != 0;
                    return STATUS_SUCCESS;
                }
                _ => {
                    // Unknown info class on a managed handle.
                    //
                    // Returning `STATUS_ACCESS_VIOLATION` here used to
                    // break real clients (notably .NET's `FileStream`
                    // init, which sets various non-essential info
                    // classes on every open and surfaces our
                    // `0xC0000005` as:
                    //
                    //   "IOException: Invalid access to memory location"
                    //
                    // `STATUS_SUCCESS` is the least-surprising fallback
                    // for metadata we don't model. The caller asked us
                    // to set state we don't track; we pretend the set
                    // took so the caller can move on.
                    tracing::warn!(
                        path = handle_context.path,
                        "nt_set_information_file_hook: file_information_class: {:?} not implemented; reporting STATUS_SUCCESS",
                        file_information_class,
                    );
                    return STATUS_SUCCESS;
                }
            }
        }

        let original = NT_SET_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            file_information_class,
        )
    }
}

//! Body of `nt_close_hook`. The hook in the
//! [FS-hook dispatcher](super::super) is a thin delegate into
//! [`handle`].
//!
//! [`handle`] is the function responsible for sending a "close file
//! descriptor" event to the remote pod, and it is also responsible of
//! removing the
//! [`HandleContext`](crate::hooks::files::managed_handle::HandleContext)
//! for `handle` from the
//! [`crate::hooks::files::managed_handle::MANAGED_HANDLES`]
//! structure.
//!
//! Additionally drops any IOCP binding the file is a part of via
//! [`iocp::unbind_file`] before the file entry is removed. Ports
//! themselves are real OS handles owned end-to-end by the OS, and the
//! layer never tracks them, so closing a port handle needs no
//! layer-side cleanup.
//!
//! As a result, all future operations done on our [`HANDLE`] will be
//! invalid.
//!
//! Due to details of the
//! [`crate::hooks::files::managed_handle::MANAGED_HANDLES`]
//! structure management, a [`HANDLE`] value can never be reclaimed, so
//! all cases of reusage must be treated as a bug.
//!
//! All instances of a failed network operation are marked by the return
//! value of [`STATUS_UNEXPECTED_NETWORK_ERROR`].

use mirrord_layer_lib::proxy_connection::make_proxy_request_no_response;
use mirrord_protocol::file::CloseFileRequest;
use winapi::shared::{
    ntdef::{HANDLE, NTSTATUS},
    ntstatus::{STATUS_SUCCESS, STATUS_UNEXPECTED_NETWORK_ERROR},
};

use crate::{
    hooks::files::{managed_handle::MANAGED_HANDLES, types::NT_CLOSE_ORIGINAL},
    iocp,
};

/// Body of `nt_close_hook`.
pub(in crate::hooks::files) unsafe fn handle(handle: HANDLE) -> NTSTATUS {
    unsafe {
        if let Ok(mut handles) = MANAGED_HANDLES.try_write()
            && let Some(managed_handle) = handles.get(&handle)
            && let Ok(handle_context) = managed_handle.clone().try_read()
        {
            // Drop any IOCP binding before removing the file entry.
            iocp::unbind_file(handle);

            let req = make_proxy_request_no_response(CloseFileRequest {
                fd: handle_context.fd,
            });

            // Remove entry regardless of agent response so we don't leak.
            handles.remove_entry(&handle);

            if req.is_err() {
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

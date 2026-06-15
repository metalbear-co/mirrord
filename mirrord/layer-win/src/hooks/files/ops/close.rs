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
//! Any IOCP binding the file holds lives in its `HandleContext`, so removing
//! the entry drops the binding with it -- no separate unbind step. Ports
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

use crate::hooks::files::{managed_handle::MANAGED_HANDLES, types::NT_CLOSE_ORIGINAL};

/// Body of `nt_close_hook`.
pub(in crate::hooks::files) unsafe fn handle(handle: HANDLE) -> NTSTATUS {
    unsafe {
        if let Some(managed_handle) = MANAGED_HANDLES.get(&handle)
            && let Ok(handle_context) = managed_handle.try_read()
        {
            let req = make_proxy_request_no_response(CloseFileRequest {
                fd: handle_context.fd,
            });

            // Remove the entry (and with it any IOCP binding) regardless of the
            // agent response, so we don't leak.
            MANAGED_HANDLES.remove(&handle);

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

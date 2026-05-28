//! Body of `nt_cancel_io_file_hook`. The hook in the
//! [FS-hook dispatcher](super::super) is a thin delegate into
//! [`handle`].
//!
//! [`handle`] preserves the NT "one IO → exactly one completion
//! packet" invariant for managed files bound to an IOCP.
//!
//! The OS implements cancel by signaling outstanding IRPs to complete
//! with `STATUS_CANCELLED`. We have no real IRPs, every async
//! `NtReadFile` either completed inline (worker beat the cancel) or
//! has a packet in flight from [`iocp::worker`]. Either way the user's
//! next dequeue will see exactly one packet per outstanding read, so
//! there is nothing to cancel on our side. Return `STATUS_SUCCESS` and
//! let any in-flight workers carry their packets through normally.
//!
//! For unmanaged files, pass through to the original `NtCancelIoFile`
//! unchanged.

use phnt::ffi::_IO_STATUS_BLOCK;
use winapi::shared::{
    ntdef::{HANDLE, NTSTATUS},
    ntstatus::STATUS_SUCCESS,
};

use crate::{
    hooks::files::{iosb::write_iosb_success, types::NT_CANCEL_IO_FILE_ORIGINAL},
    iocp,
};

/// Body of `nt_cancel_io_file_hook`.
pub(in crate::hooks::files) unsafe fn handle(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
) -> NTSTATUS {
    unsafe {
        if iocp::binding_for_file(file).is_some() {
            // NOTE: stamp the IOSB with success and report success
            // upwards. The layer has no real IRPs to cancel, every
            // outstanding async read either already completed inline
            // (the worker beat the cancel) or has a completion packet
            // in flight from `iocp::worker`. Both cases preserve the
            // "one IO -> exactly one completion packet" invariant the
            // caller's next dequeue depends on.
            //
            // `STATUS_CANCELLED` is the value the OS would put in the
            // IOSB if cancel won the race against a real IRP; we never
            // emit it because we never have a pending IRP to win
            // against.
            tracing::debug!(
                handle = ?file,
                "nt_cancel_io_file_hook: managed file is IOCP-bound, acknowledging cancel with STATUS_SUCCESS"
            );
            write_iosb_success(io_status_block, 0);
            return STATUS_SUCCESS;
        }
        let original = NT_CANCEL_IO_FILE_ORIGINAL.get().unwrap();
        original(file, io_status_block)
    }
}

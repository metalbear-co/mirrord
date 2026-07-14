//! Body of `nt_wait_for_single_object_hook` for managed file handles.
//! The hook in the [FS-hook dispatcher](super::super) is a thin
//! delegate into [`handle`].
//!
//! [`handle`] makes `NtWaitForSingleObject` work on managed file
//! handles, which are synthetic `0x5000_xxxx` values, not real OS
//! handles, so the unmodified syscall returns `STATUS_INVALID_HANDLE`.
//!
//! Since the layer completes async reads inline (the worker writes the
//! caller's buffer + IOSB before queueing the completion packet, or in
//! the no-port case writes them entirely synchronously), by the time a
//! caller reaches `NtWaitForSingleObject(file)` the data is already in
//! place. Returning [`STATUS_WAIT_0`] immediately is therefore both
//! correct and the same answer the OS would give once its IRP
//! completed. ([`STATUS_WAIT_0`] is `0x00000000`, the same numeric
//! value as Win32's `WAIT_OBJECT_0`; both spellings mean "the wait
//! was satisfied because the object became signaled.")
//!
//! (`Test_SynchronizeAccessWithoutSyncFlag` exercises this exact path:
//! open async, NtReadFile, NtWaitForSingleObject(file), use the buffer.)
//!
//! For non-managed handles, falls through to the original
//! `NtWaitForSingleObject`.

use winapi::{
    shared::ntdef::{BOOLEAN, HANDLE, NTSTATUS, PLARGE_INTEGER},
    um::winnt::STATUS_WAIT_0,
};

use crate::hooks::files::{
    managed_handle::MANAGED_HANDLES, types::NT_WAIT_FOR_SINGLE_OBJECT_ORIGINAL,
};

/// Body of `nt_wait_for_single_object_hook`.
pub(in crate::hooks::files) unsafe fn handle(
    handle: HANDLE,
    alertable: BOOLEAN,
    timeout: PLARGE_INTEGER,
) -> NTSTATUS {
    unsafe {
        if MANAGED_HANDLES.get(&handle).is_some() {
            tracing::debug!(
                handle = ?handle,
                "nt_wait_for_single_object_hook: managed file handle, returning STATUS_WAIT_0 (data is already in the caller's buffer)"
            );
            return STATUS_WAIT_0 as NTSTATUS;
        }
        let original = NT_WAIT_FOR_SINGLE_OBJECT_ORIGINAL.get().unwrap();
        original(handle, alertable, timeout)
    }
}

//! `IO_STATUS_BLOCK` writers + pointer pre-flight.
//!
//! Almost every FS hook in `ops/` needs to:
//!
//! 1. Validate the caller-supplied `buffer` / `io_status_block` pointers and return
//!    [`STATUS_ACCESS_VIOLATION`] on invalid memory (so we don't dereference into junk the OS would
//!    have rejected anyway).
//! 2. Stuff `status` + `information` into an `IO_STATUS_BLOCK`, a bindgen-generated struct whose
//!    first field is a union, so the `__bindgen_anon_1.Status = ManuallyDrop::new(...)` dance is
//!    verbose at every site.
//!
//! Centralizing both keeps the hook bodies legible and means a future
//! change to either pattern (e.g. adding tracing on every invalid
//! pointer, or migrating off `ManuallyDrop` when the bindings update)
//! lives in one place.

use std::mem::ManuallyDrop;

use phnt::ffi::_IO_STATUS_BLOCK;
use winapi::shared::{
    ntdef::{NTSTATUS, PVOID},
    ntstatus::{STATUS_ACCESS_VIOLATION, STATUS_END_OF_FILE, STATUS_SUCCESS},
};

use crate::process::memory::is_memory_valid;

/// Validates IO pointers for an Nt operation. Lets each hook do
/// `?`-style flow into a hook-level error return.
///
/// # Arguments
///
/// * `buf` - User buffer pointer (e.g. `NtReadFile`'s `Buffer` or `NtSetInformationFile`'s
///   `FileInformation`). May be `null_mut()` when the operation legitimately has no buffer (e.g. an
///   info-set call with `Length == 0`); the null case is **not** validated and passes through as
///   `Ok(())`.
/// * `iosb` - The caller-supplied `IO_STATUS_BLOCK` slot the hook intends to write completion
///   status into. Always validated; syscalls always have one.
///
/// # Returns
///
/// * `Ok(())` - Both pointers are in commit-state memory and safe to dereference / write through.
/// * `Err(NTSTATUS)` - One of the pointers is invalid. Always [`STATUS_ACCESS_VIOLATION`] today
///   (it's the NT-documented response to bad user memory); a warning trace is also emitted naming
///   which pointer failed.
pub(in crate::hooks::files) fn check_io_pointers(
    buf: PVOID,
    iosb: *mut _IO_STATUS_BLOCK,
) -> Result<(), NTSTATUS> {
    if !buf.is_null() && !is_memory_valid(buf) {
        tracing::warn!("hook: invalid memory for buffer pointer ({:p})", buf);
        return Err(STATUS_ACCESS_VIOLATION);
    }
    if !is_memory_valid(iosb) {
        tracing::warn!(
            "hook: invalid memory for IoStatusBlock pointer ({:p})",
            iosb
        );
        return Err(STATUS_ACCESS_VIOLATION);
    }
    Ok(())
}

/// Write `status` + `information` into an `IO_STATUS_BLOCK`, hiding
/// the bindgen `ManuallyDrop<union>` ceremony. No-op on null `iosb`.
///
/// SAFETY: caller must pass a valid pointer (use
/// [`check_io_pointers`] first, or pre-validate against the NT
/// contract on a freshly-allocated IOSB).
pub(in crate::hooks::files) unsafe fn write_iosb(
    iosb: *mut _IO_STATUS_BLOCK,
    status: NTSTATUS,
    information: usize,
) {
    if iosb.is_null() {
        return;
    }
    unsafe {
        (*iosb).__bindgen_anon_1.Status = ManuallyDrop::new(status);
        (*iosb).Information = information as _;
    }
}

/// Shortcut: write `(STATUS_SUCCESS, bytes_copied)`.
///
/// SAFETY: same as [`write_iosb`].
pub(in crate::hooks::files) unsafe fn write_iosb_success(
    iosb: *mut _IO_STATUS_BLOCK,
    bytes_copied: usize,
) {
    unsafe { write_iosb(iosb, STATUS_SUCCESS, bytes_copied) };
}

/// Shortcut: write `(STATUS_END_OF_FILE, 0)`.
///
/// SAFETY: same as [`write_iosb`].
pub(in crate::hooks::files) unsafe fn write_iosb_eof(iosb: *mut _IO_STATUS_BLOCK) {
    unsafe { write_iosb(iosb, STATUS_END_OF_FILE, 0) };
}

//! File-handle-specific shim over the generic [`crate::managed`] module.
//!
//! The registry/counter/locking machinery lives in [`crate::managed`];
//! this file picks the file-domain newtype ([`MirrordFileHandle`]), the
//! per-handle context struct ([`HandleContext`]), and the singleton
//! [`MANAGED_FILES`].

use std::{borrow::Borrow, ops::Deref};

use once_cell::sync::Lazy;
use str_win::path_to_unix_path;
use winapi::{
    shared::{
        minwindef::{FILETIME, ULONG},
        ntdef::{HANDLE, POBJECT_ATTRIBUTES},
    },
    um::winnt::ACCESS_MASK,
};

use crate::{
    hooks::files::util::read_object_attributes_name,
    managed::{
        ManagedRegistry,
        handle::{CounterAllocated, ManagedHandleKey},
    },
};

/// A [`HANDLE`] value the layer hands out itself, distinguishing
/// `nt_create_file_hook`-allocated handles from any other handle the user
/// might pass to file syscalls. Values come from a monotonic counter
/// starting at [`MIRRORD_FIRST_FILE_HANDLE`].
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::hooks::files) struct MirrordFileHandle(HANDLE);

unsafe impl Send for MirrordFileHandle {}
unsafe impl Sync for MirrordFileHandle {}

impl MirrordFileHandle {
    /// Borrow the raw `HANDLE` value. Used by tracing call sites that
    /// need the integer form of the handle for log lines.
    pub(in crate::hooks::files) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Borrow<HANDLE> for MirrordFileHandle {
    fn borrow(&self) -> &HANDLE {
        &self.0
    }
}

impl Deref for MirrordFileHandle {
    type Target = HANDLE;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Starting value for the file-handle counter. Picked far enough from the
/// OS handle space that collisions are not a concern, and gives every
/// managed file handle a `0x5000_xxxx`-shaped value that's recognizable
/// in a debugger or log line.
const MIRRORD_FIRST_FILE_HANDLE: usize = 0x5000_0000;

impl ManagedHandleKey for MirrordFileHandle {}

impl CounterAllocated for MirrordFileHandle {
    const FIRST: usize = MIRRORD_FIRST_FILE_HANDLE;

    fn from_raw(raw: usize) -> Self {
        Self(raw as _)
    }
}

/// Singleton registry of every file handle the layer is currently
/// tracking. Inserts go through [`try_insert_handle`]; lookups are bare
/// `MANAGED_FILES.try_read().ok()` calls scattered through the file hooks.
pub(in crate::hooks::files) static MANAGED_FILES: Lazy<
    ManagedRegistry<MirrordFileHandle, HandleContext>,
> = Lazy::new(ManagedRegistry::new);

/// Alias used by every [`hooks::files::ops`](crate::hooks::files::ops)
/// module. `MANAGED_FILES` is the canonical name; `MANAGED_HANDLES`
/// is the historical spelling the hook bodies were written against.
/// Both resolve to the same `static`.
pub(in crate::hooks::files) use self::MANAGED_FILES as MANAGED_HANDLES;

/// The data behind a [`MirrordFileHandle`].
pub(in crate::hooks::files) struct HandleContext {
    /// The Linux path, maps to the `fd`
    pub(in crate::hooks::files) path: String,
    /// Remote file descriptor for file
    pub(in crate::hooks::files) fd: u64,
    /// Windows desired access
    pub(in crate::hooks::files) desired_access: ACCESS_MASK,
    /// Windows file attributes
    pub(in crate::hooks::files) file_attributes: ULONG,
    /// Windows share access
    #[allow(dead_code)]
    pub(in crate::hooks::files) share_access: ULONG,
    /// Windows create disposition
    #[allow(dead_code)]
    pub(in crate::hooks::files) create_disposition: ULONG,
    /// Windows create options
    pub(in crate::hooks::files) create_options: ULONG,
    /// Creation time as [`FILETIME`]
    pub(in crate::hooks::files) creation_time: FILETIME,
    /// Access time as [`FILETIME`]
    pub(in crate::hooks::files) access_time: FILETIME,
    /// Write time as [`FILETIME`]
    pub(in crate::hooks::files) write_time: FILETIME,
    /// Change time as [`FILETIME`]
    pub(in crate::hooks::files) change_time: FILETIME,
    /// `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`. Set via
    /// `NtSetInformationFile(FileIoCompletionNotificationInformation)`.
    ///
    /// When `true`, async reads on this file that would have completed
    /// inline with `STATUS_SUCCESS` do NOT enqueue a packet to the bound
    /// port.
    ///
    /// Matches the high-throughput-IO opt-in tested by
    /// `Test_SkipPortOnSuccess`.
    pub(in crate::hooks::files) skip_on_success: bool,
}

/// Try to allocate the next [`MirrordFileHandle`] from the
/// [`MANAGED_FILES`] counter and register `handle_context` under it.
///
/// Notes:
///
/// * Handle values increment linearly starting at [`MIRRORD_FIRST_FILE_HANDLE`]. They are never
///   recycled, so any future use of an inserted-then-removed handle value indicates a bug somewhere
///   downstream of this call.
/// * `None` means "no managed handle was allocated, fall back to the original syscall." Callers in
///   hook context should treat the result as a hint to defer to the unmodified `NtCreateFile`
///   rather than waiting on the lock (we never block inside a hook).
///
/// # Arguments
///
/// * `handle_context` - The freshly-built [`HandleContext`] to attach to the new handle. Consumed
///   because the registry takes ownership.
///
/// # Returns
///
/// * `Some(MirrordFileHandle)` - Insertion succeeded; the returned key is the synthetic
///   `0x5000_xxxx` handle value the caller should write into the user's `PHANDLE` out-parameter.
/// * `None` - The registry's outer write lock was contended at try time. The handle was NOT
///   inserted; nothing to clean up.
pub(in crate::hooks::files) fn try_insert_handle(
    handle_context: HandleContext,
) -> Option<MirrordFileHandle> {
    let path = handle_context.path.clone();
    let fd = handle_context.fd;
    let result = MANAGED_FILES.try_insert(handle_context);
    match result {
        Some(h) => tracing::info!(
            handle = ?h.0, fd, path,
            "managed_handle::try_insert_handle: registered file handle"
        ),
        None => tracing::error!(
            fd,
            path,
            "managed_handle::try_insert_handle: MANAGED_FILES write lock contended, falling back"
        ),
    }
    result
}

/// Run `fun` closure over each handle whose path matches the `object_attributes`.
///
/// # Arguments
///
/// * `object_attributes` - The function should be used in the context of NT hooks where you're
///   provided a [`POBJECT_ATTRIBUTES`] structure instead of a [`HANDLE`].
/// * `fun` - Anything but.
pub(in crate::hooks::files) fn for_each_handle_with_path(
    object_attributes: POBJECT_ATTRIBUTES,
    mut fun: impl FnMut(&MirrordFileHandle, &HandleContext),
) -> bool {
    let mut any = false;

    let name = read_object_attributes_name(object_attributes);
    if let Some(linux_name) = path_to_unix_path(name)
        && let Ok(handles) = MANAGED_FILES.try_read()
    {
        for (handle, handle_context) in handles.iter() {
            if let Ok(handle_context) = handle_context.clone().try_read()
                && handle_context.path == linux_name
            {
                fun(handle, &handle_context);
                any = true;
            }
        }
    }

    any
}

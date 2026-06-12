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
        ntdef::{HANDLE, NTSTATUS, POBJECT_ATTRIBUTES},
        ntstatus::{STATUS_INVALID_PARAMETER, STATUS_SUCCESS},
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
/// tracking. Inserts go through [`insert_handle`]; lookups are
/// `MANAGED_FILES.get(..)` calls scattered through the file hooks.
pub(in crate::hooks::files) static MANAGED_FILES: Lazy<
    ManagedRegistry<MirrordFileHandle, HandleContext>,
> = Lazy::new(|| ManagedRegistry::new("MANAGED_FILES"));

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
    /// IOCP `(port, key)` binding, set via `NtSetInformationFile(FileCompletionInformation)`.
    ///
    /// Lives here rather than in a separate map keyed by the same handle. So it is dropped
    /// atomically when the handle closes: no second lock, and no leak window between unbind and
    /// remove.
    pub(in crate::hooks::files) iocp_binding: Option<IocpBinding>,
}

/// An IOCP `(port, key)` binding for a managed file. The OS owns the port end-to-end; we only
/// remember where to post a completion packet when the file's async read finishes on a worker.
#[derive(Clone, Copy)]
pub(in crate::hooks::files) struct IocpBinding {
    pub(in crate::hooks::files) port: HANDLE,
    pub(in crate::hooks::files) key: usize,
}

// SAFETY: `port` is an opaque OS handle value we store and hand back but never dereference.
unsafe impl Send for IocpBinding {}
unsafe impl Sync for IocpBinding {}

impl HandleContext {
    /// Bind this file to an OS IOCP port.
    ///
    /// Records the `(port, key)`. The OS port itself is untouched.
    ///
    /// # Returns
    ///
    /// `STATUS_SUCCESS`, or `STATUS_INVALID_PARAMETER` if the file is already bound (the
    /// double-bind the kernel rejects, per `Test_DoubleBindRejected`).
    pub(in crate::hooks::files) fn bind_iocp(&mut self, port: HANDLE, key: usize) -> NTSTATUS {
        if self.iocp_binding.is_some() {
            tracing::warn!(
                ?port,
                "HandleContext::bind_iocp: file already bound to a port, returning STATUS_INVALID_PARAMETER"
            );
            return STATUS_INVALID_PARAMETER;
        }
        self.iocp_binding = Some(IocpBinding { port, key });
        tracing::info!(?port, key, "HandleContext::bind_iocp: bound file -> port");
        STATUS_SUCCESS
    }

    /// This file's IOCP `(port, key)` binding, if any.
    pub(in crate::hooks::files) fn iocp_binding(&self) -> Option<(HANDLE, usize)> {
        self.iocp_binding.map(|b| (b.port, b.key))
    }

    /// Drop this file's IOCP binding (idempotent). Matches
    /// `FileReplaceCompletionInformation(Port = NULL)`.
    pub(in crate::hooks::files) fn unbind_iocp(&mut self) {
        if self.iocp_binding.take().is_some() {
            tracing::info!("HandleContext::unbind_iocp: removed binding");
        }
    }
}

/// The IOCP `(port, key)` binding for a file handle, by value.
///
/// For hooks that don't already hold the file's context (e.g. `nt_cancel_io_file_hook`).
///
/// # Returns
///
/// The `(port, key)`, or `None` for an unmanaged file or one with no binding.
pub(in crate::hooks::files) fn iocp_binding_for_file(file: HANDLE) -> Option<(HANDLE, usize)> {
    let context = MANAGED_FILES.get(&file)?;
    let context = context.try_read().ok()?;
    context.iocp_binding()
}

/// Register a freshly-opened remote file. Returns its managed handle.
///
/// Allocates the next [`MirrordFileHandle`] from the [`MANAGED_FILES`] counter and stores
/// `handle_context` under it.
///
/// `handle_context` is consumed: the registry takes ownership.
///
/// # Blocking
///
/// ⚠️ This always succeeds. Under pathological contention the registry blocks briefly rather than
/// dropping the registration.
///
/// A remotely-opened file *must* be tracked. Otherwise its remote fd leaks, and the caller gets
/// back a stale local handle.
///
/// # Note
///
/// Handle values start at [`MIRRORD_FIRST_FILE_HANDLE`] and increment linearly. They are never
/// recycled. Any reuse of an inserted-then-removed value is a bug downstream.
///
/// # Returns
///
/// The new [`MirrordFileHandle`] — the `0x5000_xxxx` value the caller writes into the user's
/// `PHANDLE`.
pub(in crate::hooks::files) fn insert_handle(handle_context: HandleContext) -> MirrordFileHandle {
    let path = handle_context.path.clone();
    let fd = handle_context.fd;
    let handle = MANAGED_FILES.insert(handle_context);
    tracing::info!(
        handle = ?handle.0, fd, path,
        "managed_handle::insert_handle: registered file handle"
    );
    handle
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
    if let Some(linux_name) = path_to_unix_path(name) {
        MANAGED_FILES.for_each(|handle, handle_context| {
            if let Ok(handle_context) = handle_context.try_read()
                && handle_context.path == linux_name
            {
                fun(handle, &handle_context);
                any = true;
            }
        });
    }

    any
}

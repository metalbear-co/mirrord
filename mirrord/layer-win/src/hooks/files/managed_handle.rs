//! File-handle-specific shim over the generic [`crate::managed`] module.
//!
//! The registry/counter/locking machinery lives in [`crate::managed`];
//! this file picks the file-domain newtype ([`MirrordFileHandle`]), the
//! per-handle context struct ([`HandleContext`]), and the singleton
//! [`MANAGED_FILES`]. The hook bodies reach the registry only through the functions here, so
//! every lookup takes the cheap range check of [`managed_file`] first.

use std::{
    borrow::Borrow,
    ops::Deref,
    sync::{Arc, RwLock},
};

use once_cell::sync::Lazy;
use str_win::{UnixPath, path_to_unix_path};
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

type FileRegistry = ManagedRegistry<MirrordFileHandle, HandleContext>;

/// Singleton registry of every file handle the layer is currently tracking. Inserts go through
/// [`insert_handle`], lookups through [`managed_file`] and removals through [`remove_handle`].
static MANAGED_FILES: Lazy<FileRegistry> = Lazy::new(|| ManagedRegistry::new("MANAGED_FILES"));

/// The data behind a [`MirrordFileHandle`].
pub(in crate::hooks::files) struct HandleContext {
    /// The Linux path, maps to the `fd`
    pub(in crate::hooks::files) path: String,
    /// The path the program opened, before `feature.fs.mapping` rewrote it into [`Self::path`].
    /// A later call that names the file instead of passing this handle names it this way.
    pub(in crate::hooks::files) requested_path: UnixPath,
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
        tracing::debug!(?port, key, "HandleContext::bind_iocp: bound file -> port");
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
            tracing::debug!("HandleContext::unbind_iocp: removed binding");
        }
    }
}

/// Whether `handle` can be a file handle this layer handed out, without a lookup.
///
/// Managed handles count up from [`MIRRORD_FIRST_FILE_HANDLE`], and kernel handles are small,
/// so this turns away nearly every handle a hook sees. A `true` still needs the lookup:
/// pseudo-handles such as `GetCurrentProcess()` are large too.
#[inline]
fn may_be_managed_handle(handle: HANDLE) -> bool {
    handle as usize >= MIRRORD_FIRST_FILE_HANDLE
}

/// The context of `handle`, if it is a file handle this layer handed out.
///
/// The file hooks see every handle in the process, sockets included (`NtDeviceIoControlFile`
/// carries all Winsock I/O), and almost none of them are managed. A registry lookup hashes the
/// handle twice and takes a shard's read lock that every thread using the same handle shares, so
/// [`may_be_managed_handle`] turns the handle away before the registry is touched at all.
pub(in crate::hooks::files) fn managed_file(handle: HANDLE) -> Option<Arc<RwLock<HandleContext>>> {
    if !may_be_managed_handle(handle) {
        return None;
    }
    MANAGED_FILES.get(&handle)
}

/// Whether `handle` is a file handle this layer handed out.
///
/// The hooks that act on a handle ask this when `internal_bypass` would otherwise bypass,
/// because the kernel answers a managed handle with `STATUS_INVALID_HANDLE`.
pub(in crate::hooks::files) fn is_managed_handle(handle: HANDLE) -> bool {
    managed_file(handle).is_some()
}

/// The IOCP `(port, key)` binding for a file handle, by value.
///
/// For hooks that don't already hold the file's context (e.g. `nt_cancel_io_file_hook`).
///
/// # Returns
///
/// The `(port, key)`, or `None` for an unmanaged file or one with no binding.
pub(in crate::hooks::files) fn iocp_binding_for_file(file: HANDLE) -> Option<(HANDLE, usize)> {
    let context = managed_file(file)?;
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
    tracing::debug!(
        handle = ?handle.0, fd, path,
        "managed_handle::insert_handle: registered file handle"
    );
    handle
}

/// Forget a managed file handle, together with its IOCP binding.
pub(in crate::hooks::files) fn remove_handle(handle: HANDLE) {
    MANAGED_FILES.remove(&handle);
}

/// Run `fun` closure over each handle open on the file that `object_attributes` names.
///
/// A handle is open on that file when it was opened with the same drive and path, in any case
/// (see [`HandleContext::requested_path`]). So a file that `feature.fs.mapping` sent elsewhere is
/// still found by the name the program uses, and `C:\app.json` is not `D:\app.json`.
///
/// # Arguments
///
/// * `object_attributes` - The function should be used in the context of NT hooks where you're
///   provided a [`POBJECT_ATTRIBUTES`] structure instead of a [`HANDLE`].
/// * `fun` - Anything but.
///
/// # Returns
///
/// Whether any handle was open on the file.
pub(in crate::hooks::files) fn for_each_handle_with_path(
    object_attributes: POBJECT_ATTRIBUTES,
    mut fun: impl FnMut(&MirrordFileHandle, &HandleContext),
) -> bool {
    let mut any = false;

    let name = read_object_attributes_name(object_attributes);
    if let Some(named) = path_to_unix_path(name) {
        let named_path = named.path.to_lowercase();
        MANAGED_FILES.for_each(|handle, handle_context| {
            if let Ok(handle_context) = handle_context.try_read()
                && handle_context.requested_path.drive == named.drive
                && handle_context.requested_path.path.to_lowercase() == named_path
            {
                fun(handle, &handle_context);
                any = true;
            }
        });
    }

    any
}

#[cfg(test)]
mod tests {
    use winapi::shared::ntdef::{OBJECT_ATTRIBUTES, UNICODE_STRING};

    use super::*;

    fn context() -> HandleContext {
        let time = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        HandleContext {
            path: "/app/config.json".to_owned(),
            requested_path: UnixPath {
                drive: Some('C'),
                path: "/app/config.json".to_owned(),
            },
            fd: 3,
            desired_access: 0,
            file_attributes: 0,
            share_access: 0,
            create_disposition: 0,
            create_options: 0,
            creation_time: time,
            access_time: time,
            write_time: time,
            change_time: time,
            skip_on_success: false,
            iocp_binding: None,
        }
    }

    /// Insert, get and remove on the registry: a handle from [`insert_handle`] is found through
    /// [`managed_file`] with its context until [`remove_handle`] drops it.
    #[test]
    fn registry_round_trip() {
        let handle = insert_handle(context()).raw();
        assert_eq!(
            managed_file(handle).map(|context| context.read().unwrap().fd),
            Some(3)
        );

        remove_handle(handle);
        assert!(managed_file(handle).is_none());
    }

    /// Registers a handle the program opened as `requested`, which `feature.fs.mapping` sent to
    /// `/app/appsettings.json` on the remote.
    fn open_mapped(requested: &str) -> HANDLE {
        insert_handle(HandleContext {
            path: "/app/appsettings.json".to_owned(),
            requested_path: path_to_unix_path(requested).expect("the test path is rooted"),
            ..context()
        })
        .raw()
    }

    /// The handles [`for_each_handle_with_path`] finds for the NT path `name`.
    fn handles_open_on(name: &str) -> Vec<HANDLE> {
        // NUL-terminated like the names Windows passes, with the NUL outside `Length`.
        let mut wide = name.encode_utf16().chain([0]).collect::<Vec<_>>();
        let bytes = u16::try_from((wide.len() - 1) * 2).expect("the test path is short");
        let mut object_name = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes + 2,
            Buffer: wide.as_mut_ptr(),
        };
        // SAFETY: all-zero is a valid `OBJECT_ATTRIBUTES`: no root directory and null pointers.
        let mut attributes: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
        attributes.Length = size_of::<OBJECT_ATTRIBUTES>() as ULONG;
        attributes.ObjectName = &mut object_name;

        let mut found = Vec::new();
        for_each_handle_with_path(&mut attributes, |handle, _| found.push(handle.raw()));
        found
    }

    /// A call that names a mapped file finds its handle by the name the program used, not by
    /// the remote path the mapping chose.
    #[test]
    fn a_mapped_handle_is_found_by_the_name_the_program_opened() {
        let handle = open_mapped(r"\??\C:\Repos\mapped\appsettings.json");

        let found = handles_open_on(r"\??\C:\Repos\mapped\appsettings.json");
        remove_handle(handle);

        assert_eq!(found, [handle]);
    }

    /// File names on Windows ignore case, so a different case names the same file.
    #[test]
    fn a_handle_is_found_by_its_name_in_another_case() {
        let handle = open_mapped(r"\??\C:\Repos\cased\appsettings.json");

        let found = handles_open_on(r"\??\c:\REPOS\Cased\AppSettings.json");
        remove_handle(handle);

        assert_eq!(found, [handle]);
    }

    /// The same path on another drive is another file, even when the remote path is the same.
    #[test]
    fn a_handle_is_not_found_on_another_drive() {
        let handle = insert_handle(HandleContext {
            path: "/Repos/drive/appsettings.json".to_owned(),
            requested_path: path_to_unix_path(r"\??\C:\Repos\drive\appsettings.json")
                .expect("the test path is rooted"),
            ..context()
        })
        .raw();

        let found = handles_open_on(r"\??\D:\Repos\drive\appsettings.json");
        remove_handle(handle);

        assert!(found.is_empty(), "D: holds another file, got {found:?}");
    }
}

//! File <-> (port, key) binding map.
//!
//! Every managed file handle either has no IOCP binding or has exactly
//! one `(port, key)` pair set by `NtSetInformationFile(file,
//! FileCompletionInformation, ...)`. Removing the binding is
//! `FileReplaceCompletionInformation` with `Port = NULL` (Windows 8.1+).
//! Trying to bind a file that already has one returns
//! `STATUS_INVALID_PARAMETER` (verified by `Test_DoubleBindRejected`).
//!
//! The OS owns the IOCP itself end-to-end. We just record which
//! `(port, key)` to push to when the file's async `NtReadFile`
//! completes on a worker thread.

use std::{borrow::Borrow, collections::HashMap, sync::RwLock};

use once_cell::sync::Lazy;
use winapi::shared::{
    ntdef::HANDLE,
    ntstatus::{STATUS_INVALID_PARAMETER, STATUS_SUCCESS},
};

/// `*mut c_void` isn't `Send + Sync` by default; this newtype lets us
/// store HANDLE values in a `static` HashMap. Treated as an opaque
/// token throughout, never dereferenced through.
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct FileKey(HANDLE);
unsafe impl Send for FileKey {}
unsafe impl Sync for FileKey {}
impl Borrow<HANDLE> for FileKey {
    fn borrow(&self) -> &HANDLE {
        &self.0
    }
}

/// Per-file binding stored in [`FILE_BINDINGS`].
#[derive(Clone, Copy)]
struct FileBinding {
    /// Real OS port handle the user attached this file to. Completion
    /// packets get posted here via the original `NtSetIoCompletion`.
    port: HANDLE,
    /// User-defined completion key, echoed back to the dequeuing thread
    /// as `CompletionKey` so the worker can identify the source file.
    key: usize,
}

unsafe impl Send for FileBinding {}
unsafe impl Sync for FileBinding {}

/// One row per file currently bound to an IOCP. A file that opens,
/// binds, unbinds, and rebinds is transiently in here.
static FILE_BINDINGS: Lazy<RwLock<HashMap<FileKey, FileBinding>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Cheap lookup used by `nt_read_file_hook` to decide whether to take
/// the async (IOCP-bound) path or the existing sync path.
///
/// Returns `(port_handle, completion_key)`:
///
/// * `port_handle` is what [`super::enqueue_packet`] needs.
/// * `completion_key` surfaces to the dequeuing thread as `KeyContext`.
pub(crate) fn binding_for_file(file: HANDLE) -> Option<(HANDLE, usize)> {
    let bindings = FILE_BINDINGS.try_read().ok()?;
    bindings.get(&FileKey(file)).map(|b| (b.port, b.key))
}

/// Bind a managed file to an OS port. If the file already has a
/// binding, returns `STATUS_INVALID_PARAMETER` (per spec §3 and
/// `Test_DoubleBindRejected`). The OS port is untouched, we just
/// remember to push to it later.
pub(crate) fn bind_file_to_port(file: HANDLE, port: HANDLE, key: usize) -> i32 {
    let Ok(mut bindings) = FILE_BINDINGS.try_write() else {
        tracing::error!(
            ?file,
            ?port,
            "iocp::bind_file_to_port: FILE_BINDINGS write lock contended"
        );
        return STATUS_INVALID_PARAMETER;
    };
    let file_key = FileKey(file);
    if bindings.contains_key(&file_key) {
        tracing::warn!(
            ?file,
            ?port,
            "iocp::bind_file_to_port: file already bound to a port, returning STATUS_INVALID_PARAMETER"
        );
        return STATUS_INVALID_PARAMETER;
    }

    bindings.insert(file_key, FileBinding { port, key });
    tracing::info!(
        ?file,
        ?port,
        key,
        "iocp::bind_file_to_port: bound file -> port"
    );
    STATUS_SUCCESS
}

/// Remove a file's binding. Idempotent on already-unbound files;
/// matches `FileReplaceCompletionInformation(Port=NULL)`.
pub(crate) fn unbind_file(file: HANDLE) -> i32 {
    let Ok(mut bindings) = FILE_BINDINGS.try_write() else {
        tracing::error!(
            ?file,
            "iocp::unbind_file: FILE_BINDINGS write lock contended"
        );
        return STATUS_INVALID_PARAMETER;
    };
    if bindings.remove(&FileKey(file)).is_some() {
        tracing::info!(?file, "iocp::unbind_file: removed binding");
    } else {
        tracing::trace!(
            ?file,
            "iocp::unbind_file: no binding to remove (idempotent)"
        );
    }
    STATUS_SUCCESS
}

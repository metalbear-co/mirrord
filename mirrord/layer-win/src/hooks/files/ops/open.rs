//! Body of `nt_create_file_hook`. The hook in the
//! [FS-hook dispatcher](super::super) is a thin delegate into
//! [`handle`].
//!
//! [`handle`] intercepts attempts to open a file handle. On a
//! configured-and-matching path, it returns a synthetic
//! [`MirrordFileHandle`](crate::hooks::files::managed_handle::MirrordFileHandle)
//! (in the `0x5000_xxxx` range) backed by a remote agent fd; otherwise
//! falls through to the original `NtCreateFile`.
//!
//! ## The mechanism
//!
//! 1. **NT contract check**: `FILE_SYNCHRONOUS_IO_*` requires `SYNCHRONIZE` in `desired_access`
//!    (the kernel needs to wait on the file object on the caller's behalf). The combination without
//!    SYNCHRONIZE is rejected with [`STATUS_INVALID_PARAMETER`]; we mirror the OS so the layer
//!    doesn't silently grant a handle the OS would have refused. Matches
//!    `sync::Test_SyncFlagRequiresSynchronizeAccess`.
//! 2. **fs_hooks_enabled config** -- if disabled, jump to fallback.
//! 3. **path classification** -- only NT disk paths (`is_nt_path_disk_path`) and
//!    convertible-to-Unix paths (`str_win::path_to_unix_path`) are candidates. Root (`/`) is
//!    bypassed (frameworks sometimes open the drive root for metadata we don't model).
//! 4. **mapping + filter** -- the Unix path goes through
//!    [`FileRemapper::change_path_str`](mirrord_layer_lib::file::mapper::FileRemapper::change_path_str)
//!    and [`FileFilter::check`](mirrord_layer_lib::file::filter::FileFilter::check); the filter
//!    decides remote-open / local-open / not-found.
//! 5. **write support** -- write-capable accesses (FILE_WRITE_DATA / FILE_APPEND_DATA /
//!    GENERIC_WRITE) fall through; write IO is not yet remoted.
//! 6. **pointer validation** -- `file_handle` and `io_status_block` are checked; invalid memory
//!    falls through (not an access violation, since the original NtCreateFile would itself report).
//! 7. **agent open** -- [`mirrord_protocol::file::OpenFileRequest`] returns a remote fd; on
//!    failure, fall through.
//! 8. **managed handle registration** -- [`try_insert_handle`] allocates a `MirrordFileHandle`,
//!    builds the [`crate::hooks::files::managed_handle::HandleContext`] (fd, path, access mask,
//!    attributes, timestamps, `position = 0`, `skip_on_success = false`), and writes the synthetic
//!    handle value at `*file_handle`. Subsequent ops on this handle hit the other hooks in this
//!    module.
//!
//! ## Fallback
//!
//! Discard previous progress, proceed with original execution by the
//! operating system.
//!
//! ## Caveats
//!
//! - Currently, file-system write operations are not supported.
//! - Currently, directory operations are not supported.
//!
//! ## Notes
//!
//! mirrord handles can easily be identified by debugging because of the
//! following:
//! - They are inserted starting from the numeric value `0x50000000`.
//! - They grow in linear numeric order from the starting point.

use std::path::PathBuf;

use mirrord_layer_lib::{
    file::filter::FileMode, proxy_connection::make_proxy_request_with_response, setup::setup,
};
use mirrord_protocol::file::{OpenFileRequest, OpenOptionsInternal};
use phnt::ffi::{_IO_STATUS_BLOCK, FILE_SYNCHRONOUS_IO_ALERT, FILE_SYNCHRONOUS_IO_NONALERT};
use str_win::path_to_unix_path;
use winapi::{
    shared::{
        ntdef::{NTSTATUS, PHANDLE, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PVOID, ULONG},
        ntstatus::{STATUS_INVALID_PARAMETER, STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SUCCESS},
    },
    um::winnt::{ACCESS_MASK, FILE_APPEND_DATA, FILE_WRITE_DATA, GENERIC_WRITE, SYNCHRONIZE},
};

use crate::{
    hooks::files::{
        managed_handle::{HandleContext, try_insert_handle},
        types::NT_CREATE_FILE_ORIGINAL,
        util::{WindowsTime, is_nt_path_disk_path, read_object_attributes_name},
    },
    process::memory::is_memory_valid,
};

/// Returns `true` iff `desired_access` satisfies the NT requirement
/// that `FILE_SYNCHRONOUS_IO_{ALERT,NONALERT}` in `create_options`
/// requires `SYNCHRONIZE` in `DesiredAccess`. Returns `true` for
/// every async open (the requirement is vacuous when neither sync
/// flag is set).
fn synchronize_present_when_required(create_options: ULONG, desired_access: ACCESS_MASK) -> bool {
    let wants_sync_io =
        (create_options & (FILE_SYNCHRONOUS_IO_ALERT | FILE_SYNCHRONOUS_IO_NONALERT)) != 0;
    !wants_sync_io || (desired_access & SYNCHRONIZE) != 0
}

/// Body of `nt_create_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn handle(
    file_handle: PHANDLE,
    desired_access: ACCESS_MASK,
    object_attributes: POBJECT_ATTRIBUTES,
    io_status_block: *mut _IO_STATUS_BLOCK,
    allocation_size: PLARGE_INTEGER,
    file_attributes: ULONG,
    share_access: ULONG,
    create_disposition: ULONG,
    create_options: ULONG,
    ea_buf: PVOID,
    ea_size: ULONG,
) -> NTSTATUS {
    unsafe {
        let run_original = || -> NTSTATUS {
            let original = NT_CREATE_FILE_ORIGINAL.get().unwrap();
            original(
                file_handle,
                desired_access,
                object_attributes,
                io_status_block,
                allocation_size,
                file_attributes,
                share_access,
                create_disposition,
                create_options,
                ea_buf,
                ea_size,
            )
        };

        // 1. NT contract
        //
        // `FILE_SYNCHRONOUS_IO_{ALERT,NONALERT}` requires `SYNCHRONIZE`
        // in `DesiredAccess`. Mirror the OS so we don't silently grant
        // a handle the OS would have refused.
        if !synchronize_present_when_required(create_options, desired_access) {
            return STATUS_INVALID_PARAMETER;
        }

        // 2. fs_hooks_enabled config
        let setup = setup();
        if !setup.fs_hooks_enabled() {
            return run_original();
        }

        // 3. Path classification
        //
        // Only NT disk paths convertible to a Unix path are candidates.
        let name = read_object_attributes_name(object_attributes);

        if !is_nt_path_disk_path(&name) {
            return run_original();
        }

        let Some(parsed_unix_path) = path_to_unix_path(name.clone()) else {
            return run_original();
        };

        // Some frameworks open the drive root (e.g. `C:\`) to inspect
        // metadata. We don't remote root directory handles.
        if parsed_unix_path == "/" {
            tracing::debug!(
                "nt_create_file_hook: bypassing remote open for root directory \"{}\"",
                name
            );
            return run_original();
        }

        // 4. Mapping + filter
        //
        // Run the parsed path through the file-remapper, then ask the
        // filter whether it should be opened locally, treated as
        // not-found, or remoted (read-only / read-write).
        let mapper = setup.file_remapper();
        let unix_path = String::from(mapper.change_path_str(parsed_unix_path.as_str()));
        if parsed_unix_path != unix_path {
            tracing::info!(
                "nt_create_file_hook: mapping matched, \"{}\" -> \"{}\"",
                parsed_unix_path,
                unix_path
            );
        }

        let filter = setup.file_filter();
        match filter.check(&unix_path) {
            Some(FileMode::Local(_)) => {
                tracing::trace!("nt_create_file_hook: reading \"{}\" locally!", name);
                return run_original();
            }
            Some(FileMode::NotFound(_)) => {
                *file_handle = std::ptr::null_mut();
                *io_status_block = _IO_STATUS_BLOCK::default();
                return STATUS_OBJECT_PATH_NOT_FOUND;
            }
            // TODO(gabriela): edit when supported!
            Some(FileMode::ReadOnly(_)) | Some(FileMode::ReadWrite(_)) | None => {
                // 5. Write support
                //
                // Write-capable accesses fall through to the original
                // syscall; write IO is not yet remoted.
                if desired_access & FILE_WRITE_DATA != 0
                    || desired_access & FILE_APPEND_DATA != 0
                    || desired_access & GENERIC_WRITE != 0
                {
                    tracing::warn!(
                        path = unix_path,
                        "nt_create_file_hook: write mode not supported presently. falling back to original!"
                    );
                    return run_original();
                }
            }
        }

        // 6. Pointer validation
        //
        // `file_handle` and `io_status_block` are the caller-supplied
        // out-params we're about to write through. Invalid memory falls
        // through to the original syscall (which would itself report).
        if file_handle.is_null() {
            tracing::warn!("nt_create_file_hook: Invalid memory for file_handle variable in hook");
            return run_original();
        }
        if !is_memory_valid(io_status_block) {
            tracing::warn!(
                "nt_create_file_hook: Invalid memory for io_status_block variable in hook"
            );
            return run_original();
        }

        // 7. Agent open
        //
        // NOTE(gabriela): currently read-only.
        // TODO(gabriela): update when write mode supported.
        let open_options = OpenOptionsInternal {
            read: true,
            ..Default::default()
        };

        let req = make_proxy_request_with_response(OpenFileRequest {
            path: PathBuf::from(&unix_path),
            open_options,
        });

        let managed_handle = match req {
            Ok(Ok(file)) => {
                let current_time = WindowsTime::current().as_file_time();
                try_insert_handle(HandleContext {
                    path: unix_path.clone(),
                    fd: file.fd,
                    desired_access,
                    file_attributes,
                    share_access,
                    create_disposition,
                    create_options,
                    creation_time: current_time,
                    access_time: current_time,
                    write_time: current_time,
                    change_time: current_time,
                    skip_on_success: false,
                })
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    ?e,
                    ?unix_path,
                    "nt_create_file_hook: Request for open file failed!"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    ?e,
                    ?unix_path,
                    "nt_create_file_hook: Request for open file failed!"
                );
                None
            }
        };

        if let Some(handle) = managed_handle {
            *file_handle = *handle;
            tracing::info!(
                "nt_create_file_hook: Succesfully opened remote file handle for {} ({:8x})",
                unix_path,
                *file_handle as usize
            );
            STATUS_SUCCESS
        } else {
            tracing::info!(
                ?unix_path,
                "nt_create_file_hook: Failed opening remote file handle"
            );
            run_original()
        }
    }
}

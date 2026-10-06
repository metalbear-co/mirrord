//! Bodies of the unimplemented file hooks. The hooks in the
//! [FS-hook dispatcher](super::super) are thin delegates into each of
//! these.
//!
//! Every stub here is either a pure passthrough or a "report-if-managed,
//! then fall through to the original NT syscall" body. The report is
//! load-bearing: it's the only signal we get in production when a
//! managed-file caller hits a hook we don't yet remote, and so drives
//! the prioritization of what to implement next. Each stub keeps its own
//! call site so it can grow independently (an extra field in the trace,
//! a partial implementation, ad-hoc instrumentation during a debug
//! session, etc.).
//!
//! The handle-based stubs see every handle in the process (`NtWriteFile` every `WriteFile`,
//! `NtDeviceIoControlFile` every Winsock operation), so they ask [`managed_file`], whose range
//! check sends a handle that is not ours straight to the original. The path-based stubs have no
//! handle, so they look up the managed handles open on the same path with
//! [`for_each_handle_with_path`]. Every managed handle found either way is reported through
//! [`UNSUPPORTED_OPERATIONS`], at `warn` the first time per kind of operation and at `debug`
//! after that, so an application that loops over an unsupported call does not flood the log.
//!
//! Stubs covered here:
//!
//! - [`write`](fn@write): write IO against managed files is not yet remoted.
//! - [`set_volume_information`]: volume-level metadata writes.
//! - [`set_quota_information`]: per-user quota writes.
//! - [`query_attributes`]: path-based attribute query.
//! - [`query_quota_information`]: per-user quota queries.
//! - [`delete_file`]: path-based deletion; write IO is out of scope.
//! - [`device_io_control`]: arbitrary IOCTL; the buffers are untyped and may be huge or sensitive,
//!   so only the control code is reported.
//! - [`lock_file`] / [`unlock_file`]: range locking on managed files isn't remoted.

use std::{
    ffi::c_void,
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use phnt::ffi::{_IO_STATUS_BLOCK, FSINFOCLASS, PFILE_BASIC_INFORMATION, PIO_APC_ROUTINE};
use tracing::field;
use winapi::{
    shared::ntdef::{
        BOOLEAN, HANDLE, NTSTATUS, PLARGE_INTEGER, POBJECT_ATTRIBUTES, PULONG, PVOID, ULONG,
    },
    um::winnt::PSID,
};

use crate::hooks::files::{
    managed_handle::{for_each_handle_with_path, managed_file},
    types::{
        NT_DELETE_FILE_ORIGINAL, NT_DEVICE_IO_CONTROL_FILE_ORIGINAL, NT_LOCK_FILE_ORIGINAL,
        NT_QUERY_ATTRIBUTES_FILE_ORIGINAL, NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL,
        NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL, NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL,
        NT_UNLOCK_FILE_ORIGINAL, NT_WRITE_FILE_ORIGINAL,
    },
};

/// An operation that the layer cannot perform on a remote file. The discriminant is the index of
/// the operation's bit in [`UnsupportedOperations`], so every control code of `DeviceIoControl`
/// shares one bit.
#[derive(Clone, Copy)]
enum UnsupportedOperation {
    Write,
    SetVolumeInformation,
    SetQuotaInformation,
    QueryQuotaInformation,
    DeviceIoControl,
    LockFile,
    UnlockFile,
    QueryAttributes,
    DeleteFile,
}

impl UnsupportedOperation {
    /// The NT function the application called.
    fn name(self) -> &'static str {
        match self {
            Self::Write => "NtWriteFile",
            Self::SetVolumeInformation => "NtSetVolumeInformationFile",
            Self::SetQuotaInformation => "NtSetQuotaInformationFile",
            Self::QueryQuotaInformation => "NtQueryQuotaInformationFile",
            Self::DeviceIoControl => "NtDeviceIoControlFile",
            Self::LockFile => "NtLockFile",
            Self::UnlockFile => "NtUnlockFile",
            Self::QueryAttributes => "NtQueryAttributesFile",
            Self::DeleteFile => "NtDeleteFile",
        }
    }
}

/// Shows an IOCTL code in hex, the form in which control codes are documented and recognized.
struct IoControlCode(ULONG);

impl fmt::Display for IoControlCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

/// Which kinds of [`UnsupportedOperation`] have already been reported at `warn`.
struct UnsupportedOperations(AtomicU32);

impl UnsupportedOperations {
    const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    /// Logs that the application asked for `operation` on the remote file behind `file`. The call
    /// still goes to the original, which does not see the remote file: it fails a managed handle
    /// and acts on the local file system for a path.
    fn report(
        &self,
        operation: UnsupportedOperation,
        io_control_code: Option<ULONG>,
        file: HANDLE,
        path: Option<&str>,
    ) {
        let bit = 1 << operation as u32;
        let operation = operation.name();
        let io_control_code = io_control_code.map(|code| field::display(IoControlCode(code)));
        if self.0.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            tracing::warn!(
                operation,
                io_control_code,
                handle = ?file,
                path,
                "the application attempted an operation that mirrord does not support on a remote \
                 (mirrord-managed) file; passing it to the original, which does not see the remote \
                 file. Further attempts of this operation are logged at debug"
            );
        } else {
            tracing::debug!(
                operation,
                io_control_code,
                handle = ?file,
                path,
                "unsupported operation on a remote (mirrord-managed) file"
            );
        }
    }
}

static UNSUPPORTED_OPERATIONS: UnsupportedOperations = UnsupportedOperations::new();

/// Reports `operation` when `file` is a managed handle. Any other handle costs only the range
/// check inside [`managed_file`].
fn report_if_managed(
    operation: UnsupportedOperation,
    io_control_code: Option<ULONG>,
    file: HANDLE,
) {
    if let Some(context) = managed_file(file) {
        let path = context.try_read().ok().map(|context| context.path.clone());
        UNSUPPORTED_OPERATIONS.report(operation, io_control_code, file, path.as_deref());
    }
}

/// Reports `operation` for every managed handle open on the path in `object_attributes`.
fn report_for_each_handle_with_path(
    operation: UnsupportedOperation,
    object_attributes: POBJECT_ATTRIBUTES,
) {
    for_each_handle_with_path(object_attributes, |handle, handle_context| {
        UNSUPPORTED_OPERATIONS.report(operation, None, handle.raw(), Some(&handle_context.path));
    });
}

/// Body of `nt_write_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn write(
    file: HANDLE,
    event: HANDLE,
    apc_routine: *mut c_void,
    apc_context: PVOID,
    io_status_block: *mut c_void,
    buffer: PVOID,
    length: ULONG,
    byte_offset: PLARGE_INTEGER,
    key: PULONG,
) -> NTSTATUS {
    unsafe {
        report_if_managed(UnsupportedOperation::Write, None, file);

        let original = NT_WRITE_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            buffer,
            length,
            byte_offset,
            key,
        )
    }
}

/// Body of `nt_set_volume_information_file_hook`.
pub(in crate::hooks::files) unsafe fn set_volume_information(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    file_information: PVOID,
    length: ULONG,
    fs_info_class: FSINFOCLASS,
) -> NTSTATUS {
    unsafe {
        report_if_managed(UnsupportedOperation::SetVolumeInformation, None, file);

        let original = NT_SET_VOLUME_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            file_information,
            length,
            fs_info_class,
        )
    }
}

/// Body of `nt_set_quota_information_file_hook`.
pub(in crate::hooks::files) unsafe fn set_quota_information(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
) -> NTSTATUS {
    unsafe {
        report_if_managed(UnsupportedOperation::SetQuotaInformation, None, file);

        let original = NT_SET_QUOTA_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(file, io_status_block, buffer, length)
    }
}

/// Body of `nt_query_attributes_file_hook`.
pub(in crate::hooks::files) unsafe fn query_attributes(
    object_attributes: POBJECT_ATTRIBUTES,
    file_basic_info: PFILE_BASIC_INFORMATION,
) -> NTSTATUS {
    unsafe {
        report_for_each_handle_with_path(UnsupportedOperation::QueryAttributes, object_attributes);

        let original = NT_QUERY_ATTRIBUTES_FILE_ORIGINAL.get().unwrap();
        original(object_attributes, file_basic_info)
    }
}

/// Body of `nt_query_quota_information_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn query_quota_information(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    buffer: PVOID,
    length: ULONG,
    return_single_entry: BOOLEAN,
    sid_list: PVOID,
    sid_list_length: ULONG,
    start_sid: PSID,
    restart_scan: BOOLEAN,
) -> NTSTATUS {
    unsafe {
        report_if_managed(UnsupportedOperation::QueryQuotaInformation, None, file);

        let original = NT_QUERY_QUOTA_INFORMATION_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            io_status_block,
            buffer,
            length,
            return_single_entry,
            sid_list,
            sid_list_length,
            start_sid,
            restart_scan,
        )
    }
}

/// Body of `nt_delete_file_hook`.
pub(in crate::hooks::files) unsafe fn delete_file(
    object_attributes: POBJECT_ATTRIBUTES,
) -> NTSTATUS {
    unsafe {
        report_for_each_handle_with_path(UnsupportedOperation::DeleteFile, object_attributes);

        let original = NT_DELETE_FILE_ORIGINAL.get().unwrap();
        original(object_attributes)
    }
}

/// Body of `nt_device_io_control_file_hook`.
///
/// NOTE(gabriela): SUPER unsafe to print in! The buffers are untyped
/// (caller-defined per IOCTL) and may be huge or sensitive, so only the
/// control code is reported.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn device_io_control(
    file: HANDLE,
    event: HANDLE,
    apc_routine: PIO_APC_ROUTINE,
    apc_context: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    io_control_code: ULONG,
    input_buffer: PVOID,
    input_buffer_length: ULONG,
    output_buffer: PVOID,
    output_buffer_length: ULONG,
) -> NTSTATUS {
    unsafe {
        report_if_managed(
            UnsupportedOperation::DeviceIoControl,
            Some(io_control_code),
            file,
        );

        let original = NT_DEVICE_IO_CONTROL_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            io_control_code,
            input_buffer,
            input_buffer_length,
            output_buffer,
            output_buffer_length,
        )
    }
}

/// Body of `nt_lock_file_hook`.
#[allow(clippy::too_many_arguments)]
pub(in crate::hooks::files) unsafe fn lock_file(
    file: HANDLE,
    event: HANDLE,
    apc_routine: PIO_APC_ROUTINE,
    apc_context: PVOID,
    io_status_block: *mut _IO_STATUS_BLOCK,
    byte_offset: PLARGE_INTEGER,
    length: PLARGE_INTEGER,
    key: ULONG,
    fail_immediately: BOOLEAN,
    exclusive_lock: BOOLEAN,
) -> NTSTATUS {
    unsafe {
        report_if_managed(UnsupportedOperation::LockFile, None, file);

        let original = NT_LOCK_FILE_ORIGINAL.get().unwrap();
        original(
            file,
            event,
            apc_routine,
            apc_context,
            io_status_block,
            byte_offset,
            length,
            key,
            fail_immediately,
            exclusive_lock,
        )
    }
}

/// Body of `nt_unlock_file_hook`. See [`lock_file`].
pub(in crate::hooks::files) unsafe fn unlock_file(
    file: HANDLE,
    io_status_block: *mut _IO_STATUS_BLOCK,
    byte_offset: PLARGE_INTEGER,
    length: PLARGE_INTEGER,
    key: ULONG,
) -> NTSTATUS {
    unsafe {
        report_if_managed(UnsupportedOperation::UnlockFile, None, file);

        let original = NT_UNLOCK_FILE_ORIGINAL.get().unwrap();
        original(file, io_status_block, byte_offset, length, key)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::{
        Event, Level, Metadata, Subscriber,
        span::{Attributes, Id, Record},
    };

    use super::*;

    /// Records the level of every event.
    #[derive(Clone, Default)]
    struct LevelRecorder(Arc<Mutex<Vec<Level>>>);

    impl Subscriber for LevelRecorder {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            self.0.lock().unwrap().push(*event.metadata().level());
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn an_unsupported_operation_warns_once_per_kind() {
        let reports = UnsupportedOperations::new();
        let recorder = LevelRecorder::default();
        let file = 0x5000_0000 as HANDLE;

        tracing::subscriber::with_default(recorder.clone(), || {
            reports.report(
                UnsupportedOperation::Write,
                None,
                file,
                Some("/app/out.log"),
            );
            reports.report(
                UnsupportedOperation::Write,
                None,
                file,
                Some("/app/out.log"),
            );
            reports.report(
                UnsupportedOperation::DeviceIoControl,
                Some(0x0009_0018),
                file,
                None,
            );
            reports.report(
                UnsupportedOperation::DeviceIoControl,
                Some(0x0009_00a8),
                file,
                None,
            );
            reports.report(UnsupportedOperation::Write, None, file, None);
            reports.report(
                UnsupportedOperation::QueryAttributes,
                None,
                file,
                Some("/app/out.log"),
            );
            reports.report(
                UnsupportedOperation::QueryAttributes,
                None,
                file,
                Some("/app/out.log"),
            );
            reports.report(
                UnsupportedOperation::DeleteFile,
                None,
                file,
                Some("/app/out.log"),
            );
        });

        assert_eq!(
            *recorder.0.lock().unwrap(),
            [
                Level::WARN,
                Level::DEBUG,
                Level::WARN,
                Level::DEBUG,
                Level::DEBUG,
                Level::WARN,
                Level::DEBUG,
                Level::WARN,
            ],
        );
    }
}

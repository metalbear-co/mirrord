//! Utility module for files redirection.

use std::mem::MaybeUninit;

use mirrord_layer_lib::proxy_connection::make_proxy_request_with_response;
use mirrord_protocol::file::{MetadataInternal, SeekFileRequest, SeekFromInternal, XstatRequest};
use str_win::{GLOBAL_NAMESPACE_PATH, string_to_u16_buffer, u16_buffer_to_string};
use winapi::{
    shared::{minwindef::FILETIME, ntdef::POBJECT_ATTRIBUTES},
    um::{
        fileapi::QueryDosDeviceW,
        minwinbase::SYSTEMTIME,
        sysinfoapi::GetSystemTime,
        timezoneapi::{FileTimeToSystemTime, SystemTimeToFileTime},
    },
};

/// Attempt to run xstat on pod over file descriptor.
///
/// # Arguments
///
/// * `fd` - Remote (pod) fd for file.
pub fn try_xstat(fd: u64) -> Option<MetadataInternal> {
    let req = make_proxy_request_with_response(XstatRequest {
        path: None,
        fd: Some(fd),
        follow_symlink: true,
    });

    // If the response contains the `XstatResponse`, return the metadata.
    match req {
        Ok(Ok(res)) => Some(res.metadata),
        Ok(Err(e)) => {
            tracing::error!(?e, "Protocol: Error trying to xstat into file!");
            None
        }
        Err(e) => {
            tracing::error!(?e, "Proxy: Error trying to xstat file!");
            None
        }
    }
}

/// Attempt to run seek on pod over file descriptor.
///
/// # Arguments
///
/// * `fd` - Remote (pod) fd for file.
/// * `seek` - Enum to describe the seek configuration.
pub fn try_seek(fd: u64, seek: SeekFromInternal) -> Option<u64> {
    let seek = make_proxy_request_with_response(SeekFileRequest {
        fd,
        seek_from: seek,
    });

    match seek {
        Ok(Ok(res)) => Some(res.result_offset),
        Ok(Err(e)) => {
            tracing::error!(?e, "Protocol: Error trying to seek into file!");
            None
        }
        Err(e) => {
            tracing::error!(?e, "Proxy: Error trying to seek into file!");
            None
        }
    }
}

/// Function responsible for turning a [`OBJECT_ATTRIBUTES`] structure into a [`String`].
pub fn read_object_attributes_name(object_attributes: POBJECT_ATTRIBUTES) -> String {
    unsafe {
        let name_ustr = (*object_attributes).ObjectName;

        let buf = (*name_ustr).Buffer;
        let len = (*name_ustr).Length;

        let name = &*std::ptr::slice_from_raw_parts(buf, len as _);
        u16_buffer_to_string(name)
    }
}

/// Type to work with Windows times.
pub struct WindowsTime {
    time: SYSTEMTIME,
}

impl WindowsTime {
    /// Get [`WindowsTime`] from current system time.
    pub fn current() -> Self {
        let mut time: SYSTEMTIME = unsafe { MaybeUninit::zeroed().assume_init() };
        unsafe { GetSystemTime(&mut time) };

        Self { time }
    }

    /// Get current Windows time as [`SYSTEMTIME`].
    #[allow(dead_code)]
    pub fn as_system_time(&self) -> SYSTEMTIME {
        self.time
    }

    /// Get current Windows time as [`FILETIME`].
    pub fn as_file_time(&self) -> FILETIME {
        let mut fs_time: FILETIME = unsafe { MaybeUninit::zeroed().assume_init() };
        unsafe { SystemTimeToFileTime(&self.time, &mut fs_time) };

        fs_time
    }

    /// Get current Windows time as [`u64`] from [`FILETIME`].
    pub fn as_file_time_u64(&self) -> u64 {
        let file_time = self.as_file_time();
        ((file_time.dwHighDateTime as u64) << 32) | file_time.dwLowDateTime as u64
    }

    /// Get current Windows time as [`i64`] from [`FILETIME`].
    pub fn as_file_time_i64(&self) -> i64 {
        i64::try_from(self.as_file_time_u64()).unwrap()
    }
}

impl From<SYSTEMTIME> for WindowsTime {
    fn from(value: SYSTEMTIME) -> Self {
        Self { time: value }
    }
}

impl From<FILETIME> for WindowsTime {
    fn from(value: FILETIME) -> Self {
        let mut system_time: SYSTEMTIME = unsafe { MaybeUninit::zeroed().assume_init() };
        unsafe { FileTimeToSystemTime(&value, &mut system_time) };
        Self { time: system_time }
    }
}

impl PartialEq for WindowsTime {
    fn eq(&self, other: &Self) -> bool {
        self.as_file_time_u64() == other.as_file_time_u64()
    }
}

impl PartialEq<u64> for WindowsTime {
    fn eq(&self, other: &u64) -> bool {
        self.as_file_time_u64() == *other
    }
}

/// Check if NT path points to a harddisk.
///
/// # Arguments
///
/// * `path` - The path to check.
pub fn is_nt_path_disk_path<T: AsRef<str>>(path: T) -> bool {
    let mut path = path.as_ref();

    if !path.starts_with(GLOBAL_NAMESPACE_PATH) {
        return false;
    }

    // Remove global namespace path to normalize.
    path = &path[GLOBAL_NAMESPACE_PATH.len()..];

    // Make sure to support both \\??\\C: and \\??\\C:\abc
    let volume_path = path.split_once('\\').map(|(a, _)| a).unwrap_or(path);

    let mut path = [0u16; 512];
    let volume_path = string_to_u16_buffer(volume_path);

    let ret = unsafe {
        QueryDosDeviceW(
            volume_path.as_ptr() as _,
            path.as_mut_ptr() as _,
            path.len() as _,
        )
    };

    if ret == 0 {
        false
    } else {
        let path = u16_buffer_to_string(path).to_lowercase();

        // Multiple possible varaiations of this, but the start is always consistent.
        const DEVICE_HARDDISK: &str = "\\Device\\Harddisk";

        let prefix = DEVICE_HARDDISK.to_string().to_lowercase();
        path.starts_with(&prefix)
    }
}

/// Check if NT path refers exactly to a disk root (e.g. `\\??\\C:\`).
pub fn is_nt_path_disk_root<T: AsRef<str>>(path: T) -> bool {
    let mut path = path.as_ref();

    if !path.starts_with(GLOBAL_NAMESPACE_PATH) {
        return false;
    }

    path = &path[GLOBAL_NAMESPACE_PATH.len()..];

    // normalize to backslashes
    let normalized = path.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    let mut components = trimmed.split('\\');

    // make sure we have a non-empty drive component
    match components.next() {
        Some(drive_component) if !drive_component.is_empty() => {}
        _ => return false,
    }

    match components.next() {
        // no components left means it's a root path
        None => true,
        // a single dot component means it's also a root path
        Some(segment) if segment == "." && components.next().is_none() => true,
        // anything else means it's not a root path
        _ => false,
    }
}

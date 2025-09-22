//! Utility module for files redirection.

use std::{
    mem::MaybeUninit,
    path::{Path, PathBuf},
};

use mirrord_protocol::file::{MetadataInternal, SeekFileRequest, SeekFromInternal, XstatRequest};
use winapi::{
    shared::minwindef::FILETIME,
    um::{
        minwinbase::SYSTEMTIME,
        sysinfoapi::GetSystemTime,
        timezoneapi::{FileTimeToSystemTime, SystemTimeToFileTime},
    },
};

// This prefix is a way to explicitly indicate that we're looking in
// the global namespace for a path.
const GLOBAL_NAMESPACE_PATH: &str = r#"\??\"#;

pub fn remove_root_dir_from_path<T: AsRef<Path>>(path: T) -> Option<String> {
    let mut path = path.as_ref();

    if !path.has_root() {
        return None;
    }

    // Rust doesn't know how to separate the components in this case.
    path = path.strip_prefix(GLOBAL_NAMESPACE_PATH).ok()?;

    // Skip root dir
    let new_path: PathBuf = path.components().skip(1).collect();

    // Turn to string, replace Windows slashes to Linux slashes for ease of use.
    Some(new_path.to_str()?.to_string().replace("\\", "/"))
}

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
        _ => None,
    }
}

pub fn try_seek(fd: u64, seek: SeekFromInternal) -> Option<u64> {
    let seek = make_proxy_request_with_response(SeekFileRequest {
        fd,
        seek_from: seek,
    });

    match seek {
        Ok(Ok(res)) => Some(res.result_offset),
        _ => None,
    }
}

/// Structure to work with [`SYSTEMTIME`]-s. Supports conversions as well.
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

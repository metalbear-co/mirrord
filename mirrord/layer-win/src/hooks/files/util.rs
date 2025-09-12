//! Utility module for files redirection.

use std::{
    mem::MaybeUninit,
    path::{Path, PathBuf},
};

use mirrord_protocol::file::{MetadataInternal, XstatRequest};
use winapi::{
    shared::minwindef::FILETIME,
    um::{
        minwinbase::SYSTEMTIME,
        sysinfoapi::GetSystemTime,
        timezoneapi::SystemTimeToFileTime,
    },
};

use crate::common::make_proxy_request_with_response;

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

    // If the request failed, just return `None`.
    if req.is_err() {
        return None;
    }
    let req = req.unwrap();

    // If the response contains the `XstatResponse`, return the metadata.
    match req {
        Ok(res) => Some(res.metadata),
        _ => None,
    }
}

pub struct WindowsTime {
    pub time: SYSTEMTIME,
}

impl WindowsTime {
    pub fn current() -> Self {
        let mut time: SYSTEMTIME = unsafe { MaybeUninit::zeroed().assume_init() };
        unsafe { GetSystemTime(&mut time) };

        Self { time }
    }

    pub fn as_system_time(&self) -> SYSTEMTIME {
        self.time
    }

    pub fn as_file_time(&self) -> FILETIME {
        let mut fs_time: FILETIME = unsafe { MaybeUninit::zeroed().assume_init() };
        unsafe { SystemTimeToFileTime(&self.time, &mut fs_time) };

        fs_time
    }
}

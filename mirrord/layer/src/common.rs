//! Shared place for a few types and functions that are used everywhere by the layer.
use std::{
    ffi::CStr,
    fmt::Debug,
    ops::Not,
    path::{Path, PathBuf},
};

use libc::c_char;
use mirrord_intproxy_protocol::{IsLayerRequest, IsLayerRequestWithResponse, MessageId};
use mirrord_protocol::file::OpenOptionsInternal;
#[cfg(target_os = "macos")]
use mirrord_sip::{MIRRORD_TEMP_BIN_DIR_CANONIC_PATHBUF, MIRRORD_TEMP_BIN_DIR_PATH_BUF};
use null_terminated::Nul;
use tracing::warn;

use crate::{
    detour::{Bypass, Detour},
    error::{HookError, HookResult},
    exec_hooks::Argv,
    file::OpenOptionsInternalExt,
    socket::SHARED_SOCKETS_ENV_VAR,
    PROXY_CONNECTION,
};

/// Makes a request to the internal proxy using global [`PROXY_CONNECTION`].
/// Blocks until the proxy responds.
pub fn make_proxy_request_with_response<T>(request: T) -> HookResult<T::Response>
where
    T: IsLayerRequestWithResponse + Debug,
    T::Response: Debug,
{
    // SAFETY: mutation happens only on initialization.
    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or(HookError::CannotGetProxyConnection)?
            .make_request_with_response(request)
            .map_err(Into::into)
    }
}

/// Makes a request to the internal proxy using global [`PROXY_CONNECTION`].
/// Blocks until the request is sent.
pub fn make_proxy_request_no_response<T: IsLayerRequest + Debug>(
    request: T,
) -> HookResult<MessageId> {
    // SAFETY: mutation happens only on initialization.
    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or(HookError::CannotGetProxyConnection)?
            .make_request_no_response(request)
            .map_err(Into::into)
    }
}

/// Converts raw pointer values `P` to some other type.
///
/// ## Usage
///
/// Mainly used to convert from raw C strings (`*const c_char`) into a Rust type wrapped in
/// [`Detour`].
///
/// These conversions happen in the unsafe `hook` functions, and we pass the converted value inside
/// a [`Detour`] to defer the handling of `null` pointers (and other _invalid-ish_ values) when the
/// `ops` version of the function returns an `Error` or [`Bypass`].
pub(crate) trait CheckedInto<T>: Sized {
    /// Converts `Self` to `Detour<T>`.
    fn checked_into(self) -> Detour<T>;
}

impl<'a> CheckedInto<&'a str> for *const c_char {
    fn checked_into(self) -> Detour<&'a str> {
        let converted = (!self.is_null())
            .then(|| unsafe { CStr::from_ptr(self) })
            .map(CStr::to_str)?
            .map_err(|fail| {
                warn!("Failed converting `value` from `CStr` with {:#?}", fail);
                Bypass::CStrConversion
            })?;

        Detour::Success(converted)
    }
}

impl CheckedInto<String> for *const c_char {
    fn checked_into(self) -> Detour<String> {
        CheckedInto::<&str>::checked_into(self).map(From::from)
    }
}

#[cfg(target_os = "macos")]
pub fn strip_mirrord_path(path_str: &Path) -> Option<&Path> {
    path_str
        .strip_prefix(MIRRORD_TEMP_BIN_DIR_PATH_BUF.to_owned())
        .ok()
        .or_else(|| {
            path_str
                .strip_prefix(MIRRORD_TEMP_BIN_DIR_CANONIC_PATHBUF.to_owned())
                .ok()
        })
}

impl CheckedInto<PathBuf> for *const c_char {
    /// Do the checked conversion to str, bypass if the str starts with temp dir's path, construct
    /// a `PathBuf` out of the str.
    fn checked_into(self) -> Detour<PathBuf> {
        let str_det = CheckedInto::<&str>::checked_into(self);
        #[cfg(target_os = "macos")]
        let str_det = str_det.and_then(|path_str| {
            let optional_stripped_path = strip_mirrord_path(&Path::new(path_str));
            if let Some(stripped_path) = optional_stripped_path {
                // actually stripped, so bypass and provide a pointer to after the temp dir.
                // `stripped_path` is a reference to a later character in the same string as
                // `path_str`, `stripped_path.as_ptr()` returns a pointer to a later index
                // in the same string owned by the caller (the hooked program).
                Detour::Bypass(Bypass::FileOperationInMirrordBinTempDir(
                    stripped_path.to_string_lossy().as_ptr() as _,
                ))
            } else {
                Detour::Success(path_str) // strip is None, path not in temp dir.
            }
        });
        str_det.map(From::from)
    }
}

/// **Warning**: The implementation here expects that `*const *const c_char` be a valid,
/// null-terminated list! We're using `Nul::new_unchecked`, which doesn't check for this.
/// NOTE: It also strips shared sockets to avoid it being double set.
impl CheckedInto<Argv> for *const *const c_char {
    fn checked_into(self) -> Detour<Argv> {
        let c_list = self
            .is_null()
            .not()
            .then(|| unsafe { Nul::new_unchecked(self) })?;

        let list = c_list
            .iter()
            // Remove the last `null` pointer.
            .filter(|value| !value.is_null())
            .map(|value| unsafe { CStr::from_ptr(*value) }.to_owned())
            .filter(|value| !value.to_string_lossy().starts_with(SHARED_SOCKETS_ENV_VAR))
            .collect::<Argv>();

        Detour::Success(list)
    }
}

impl CheckedInto<OpenOptionsInternal> for *const c_char {
    fn checked_into(self) -> Detour<OpenOptionsInternal> {
        CheckedInto::<String>::checked_into(self).map(OpenOptionsInternal::from_mode)
    }
}

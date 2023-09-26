//! Shared place for a few types and functions that are used everywhere by the layer.
use std::{collections::VecDeque, ffi::CStr, path::PathBuf};

use libc::c_char;
use mirrord_intproxy::protocol::{IsLayerRequest, IsLayerRequestWithResponse, MessageId};
use mirrord_protocol::{file::OpenOptionsInternal, RemoteResult};
#[cfg(target_os = "macos")]
use mirrord_sip::{MIRRORD_TEMP_BIN_DIR_CANONIC_STRING, MIRRORD_TEMP_BIN_DIR_STRING};
use tokio::sync::oneshot;
use tracing::warn;

use crate::{
    detour::{Bypass, Detour},
    error::{HookError, HookResult},
    file::OpenOptionsInternalExt,
    PROXY_CONNECTION,
};

/// Type alias for a queue of responses from the agent, where these responses are [`RemoteResult`]s.
///
/// ## Usage
///
/// We have no identifiers for the hook requests, so if hook responses were sent asynchronously we
/// would have no way to match them back to their requests. However, requests are sent out
/// synchronously, and responses are sent back synchronously, so keeping them in order is how we
/// maintain our way to match them.
///
/// - The usual flow is:
///
/// 1. `push_back` the [`oneshot::Sender`] that will be used to produce the [`RemoteResult`];
///
/// 2. When the operation gets a response from the agent:
///     1. `pop_front` to get the [`oneshot::Sender`], then;
///     2. `Sender::send` the result back to the operation that initiated the request.
pub(crate) type ResponseDeque<T> = VecDeque<ResponseChannel<T>>;

/// Type alias for the channel that sends a response from the agent.
///
/// See [`ResponseDeque`] for usage details.
pub(crate) type ResponseChannel<T> = oneshot::Sender<RemoteResult<T>>;

pub fn make_proxy_request_with_response<T: IsLayerRequestWithResponse>(
    request: T,
) -> HookResult<T::Response> {
    // SAFETY: mutation happens only on initialization.
    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or(HookError::CannotGetProxyConnection)?
            .make_request_with_response(request)
            .map_err(Into::into)
    }
}

pub fn make_proxy_request_no_response<T: IsLayerRequest>(request: T) -> HookResult<MessageId> {
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

impl CheckedInto<PathBuf> for *const c_char {
    /// Do the checked conversion to str, bypass if the str starts with temp dir's path, construct
    /// a `PathBuf` out of the str.
    fn checked_into(self) -> Detour<PathBuf> {
        let str_det = CheckedInto::<&str>::checked_into(self);
        #[cfg(target_os = "macos")]
        let str_det = str_det.and_then(|path_str| {
            let optional_stripped_path = path_str
                .strip_prefix(MIRRORD_TEMP_BIN_DIR_STRING.as_str())
                .or_else(|| path_str.strip_prefix(MIRRORD_TEMP_BIN_DIR_CANONIC_STRING.as_str()));
            if let Some(stripped_path) = optional_stripped_path {
                // actually stripped, so bypass and provide a pointer to after the temp dir.
                // `stripped_path` is a reference to a later character in the same string as
                // `path_str`, `stripped_path.as_ptr()` returns a pointer to a later index
                // in the same string owned by the caller (the hooked program).
                Detour::Bypass(Bypass::FileOperationInMirrordBinTempDir(
                    stripped_path.as_ptr() as _,
                ))
            } else {
                Detour::Success(path_str) // strip is None, path not in temp dir.
            }
        });
        str_det.map(From::from)
    }
}

impl CheckedInto<OpenOptionsInternal> for *const c_char {
    fn checked_into(self) -> Detour<OpenOptionsInternal> {
        CheckedInto::<String>::checked_into(self).map(OpenOptionsInternal::from_mode)
    }
}

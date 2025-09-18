//! Shared place for a few types and functions that are used everywhere by the layer.
use std::{ffi::CStr, ops::Not, path::PathBuf};

use libc::c_char;
// Re-export proxy request functions from layer-lib (Unix only)
pub use mirrord_layer_lib::proxy_connection::{
    make_proxy_request_no_response, make_proxy_request_with_response,
};
use mirrord_protocol::file::OpenOptionsInternal;
use null_terminated::Nul;
use tracing::warn;

use crate::{
    detour::{Bypass, Detour},
    error::HookError,
    exec_hooks::Argv,
    file::OpenOptionsInternalExt,
    socket::SHARED_SOCKETS_ENV_VAR,
};

/// Convert NulError to Detour for `?` operator support
impl<T> From<std::ffi::NulError> for Detour<T> {
    fn from(error: std::ffi::NulError) -> Self {
        Detour::Error(HookError::Null(error))
    }
}

/// Convert TryFromIntError to Detour for `?` operator support
impl<T> From<std::num::TryFromIntError> for Detour<T> {
    fn from(error: std::num::TryFromIntError) -> Self {
        Detour::Error(HookError::TryFromInt(error))
    }
}

/// Handle nested Result<Result<T, ResponseError>, HookError> structures
/// This is the pattern returned by make_proxy_request_with_response where the inner response can be
/// an error
impl<T, E> From<Result<Result<T, E>, HookError>> for Detour<T>
where
    HookError: From<E>,
{
    fn from(result: Result<Result<T, E>, HookError>) -> Self {
        match result {
            Ok(Ok(value)) => Detour::Success(value),
            Ok(Err(inner_error)) => Detour::Error(HookError::from(inner_error)),
            Err(outer_error) => Detour::Error(outer_error),
        }
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
pub fn strip_mirrord_path(path_str: &str) -> Option<&str> {
    use mirrord_sip::MIRRORD_PATCH_DIR;

    // SAFETY: We only slice after we find the string in the path
    // so it must be valid
    #[allow(clippy::indexing_slicing)]
    path_str
        .find(MIRRORD_PATCH_DIR)
        .map(|index| &path_str[(MIRRORD_PATCH_DIR.len() + index)..])
}

impl CheckedInto<PathBuf> for *const c_char {
    /// Do the checked conversion to str, bypass if the str starts with temp dir's path, construct
    /// a `PathBuf` out of the str.
    fn checked_into(self) -> Detour<PathBuf> {
        let str_det = CheckedInto::<&str>::checked_into(self);
        #[cfg(target_os = "macos")]
        let str_det = str_det.and_then(|path_str| {
            let optional_stripped_path = strip_mirrord_path(path_str);
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

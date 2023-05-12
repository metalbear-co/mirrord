//! Shared place for a few types and functions that are used everywhere by the layer.
use std::{collections::VecDeque, ffi::CStr, path::PathBuf};

#[cfg(target_os = "macos")]
use lazy_static::lazy_static;
use libc::c_char;
use mirrord_protocol::{file::OpenOptionsInternal, RemoteResult};
#[cfg(target_os = "macos")]
use mirrord_sip::MIRRORD_TEMP_BIN_DIR;
use tokio::sync::oneshot;
use tracing::warn;

use crate::{
    detour::{Bypass, Detour},
    dns::GetAddrInfo,
    error::{HookError, HookResult},
    file::{FileOperation, OpenOptionsInternalExt},
    outgoing::{tcp::TcpOutgoing, udp::UdpOutgoing},
    tcp::TcpIncoming,
    HOOK_SENDER,
};

#[cfg(target_os = "macos")]
lazy_static! {
    /// Path of current executable, None if fetching failed.
    pub static ref CURRENT_EXE: Option<String> = std::env::current_exe().ok().map(|path_buf| path_buf.to_string_lossy().to_string());
}

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

/// Sends a [`HookMessage`] through the global [`HOOK_SENDER`] channel.
///
/// ## Flow
///
/// hook function -> [`HookMessage`] -> [`blocking_send_hook_message`] -> [`ClientMessage`]
///
/// ## Usage
///
/// - [`file::ops`](crate::file::ops): most of the file operations are blocking, and thus this
///   function is extensively used there;
///
/// - [`socket::ops`](crate::socket::ops): used by some functions that are _blocking-ish_.
///
/// [`ClientMessage`]: mirrord_protocol::codec::ClientMessage
pub(crate) fn blocking_send_hook_message(message: HookMessage) -> HookResult<()> {
    HOOK_SENDER
        .get()
        .ok_or(HookError::EmptyHookSender)
        .and_then(|hook_sender| hook_sender.blocking_send(message).map_err(Into::into))
}

/// These messages are handled internally by the layer, and become `ClientMessage`s sent to
/// the agent.
///
/// Most hook detours will send a [`HookMessage`] that will be converted to an equivalent
/// `ClientMessage` after some internal handling is done. Usually this means taking a sender
/// channel from this message, and pushing it into a [`ResponseDeque`], while taking the other
/// fields of the message to become a `ClientMessage`.
#[derive(Debug)]
pub(crate) enum HookMessage {
    /// TCP incoming messages originating from a hook, see [`TcpIncoming`].
    Tcp(TcpIncoming),

    /// TCP outgoing messages originating from a hook, see [`TcpOutgoing`].
    TcpOutgoing(TcpOutgoing),

    /// UDP outgoing messages originating from a hook, see [`UdpOutgoing`].
    UdpOutgoing(UdpOutgoing),

    /// File messages originating from a hook, see [`FileOperation`].
    File(FileOperation),

    /// Message originating from `getaddrinfo`, see [`GetAddrInfo`].
    GetAddrinfo(GetAddrInfo),
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

/// For a given str, return whether it's the path of the current running executable.
///
/// Also returns false if determining the current executable failed, or if its path is non-unicode.
#[cfg(target_os = "macos")]
fn is_current_exe(path: &str) -> bool {
    CURRENT_EXE
        .as_deref()
        .map(|exe_string| exe_string == path)
        .unwrap_or_default()
}

impl CheckedInto<PathBuf> for *const c_char {
    /// Do the checked conversion to str, bypass if the str starts with temp dir's path, construct
    /// a `PathBuf` out of the str.
    fn checked_into(self) -> Detour<PathBuf> {
        let str_det = CheckedInto::<&str>::checked_into(self);
        #[cfg(target_os = "macos")]
        let str_det = str_det.and_then(|path_str| {
            if let Some(stripped_path) = path_str.strip_prefix(MIRRORD_TEMP_BIN_DIR.as_str())
                && !is_current_exe(path_str) {
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

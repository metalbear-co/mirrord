//! The layer uses features from this module to check if it should bypass one of its hooks, and call
//! the original `libc` function.
//!
//! Here we also have the convenient `Detour`, that is used by the hooks to either return a
//! [`Result`]-like value, or the special [`Bypass`] case, which makes the _detour_ function call
//! the original [`libc`] equivalent, stored in a `HookFn`.

use std::net::SocketAddr;
#[cfg(unix)]
use std::{cell::RefCell, ffi::CString, os::fd::RawFd, path::PathBuf};

#[cfg(target_os = "macos")]
use libc::c_char;

#[cfg(unix)]
thread_local!(
    /// Holds the thread-local state for bypassing the layer's detour functions.
    ///
    /// ## Warning
    ///
    /// Do **NOT** use this directly, instead use `DetourGuard::new` if you need to
    /// create a bypass inside a function (like we have in
    /// [`TcpHandler::create_local_stream`](crate::tcp::TcpHandler::create_local_stream)).
    ///
    /// Or rely on the [`hook_guard_fn`](mirrord_layer_macro::hook_guard_fn) macro.
    ///
    /// ## Details
    ///
    /// Some of the layer functions will interact with [`libc`] functions that we are hooking into,
    /// thus we could end up _stealing_ a call by the layer itself rather than by the binary the
    /// layer is injected into. An example of this  would be if we wanted to open a file locally,
    /// the layer's `open_detour` intercepts the [`libc::open`] call, and we get a remote file
    /// (if it exists), instead of the local file we wanted.
    ///
    /// We set this to `true` whenever an operation may require calling other [`libc`] functions,
    /// and back to `false` after it's done.
    static DETOUR_BYPASS: RefCell<bool> = const { RefCell::new(false) }
);

/// Sets [`DETOUR_BYPASS`] to `false`.
///
/// Prefer relying on the [`Drop`] implementation of [`DetourGuard`] instead.
#[cfg(unix)]
pub(super) fn detour_bypass_off() {
    DETOUR_BYPASS.with(|enabled| {
        if let Ok(mut bypass) = enabled.try_borrow_mut() {
            *bypass = false
        }
    });
}

/// Handler for the layer's [`DETOUR_BYPASS`].
///
/// Sets [`DETOUR_BYPASS`] on creation, and turns it off on [`Drop`].
///
/// ## Warning
///
/// You should always use `DetourGuard::new`, if you construct this in any other way, it's
/// not going to guard anything.
#[cfg(unix)]
pub struct DetourGuard;

#[cfg(unix)]
impl DetourGuard {
    /// Create a new DetourGuard if it's not already enabled.
    pub fn new() -> Option<Self> {
        DETOUR_BYPASS.with(|enabled| {
            if let Ok(bypass) = enabled.try_borrow()
                && *bypass
            {
                None
            } else {
                match enabled.try_borrow_mut() {
                    Ok(mut bypass) => {
                        *bypass = true;
                        Some(Self)
                    }
                    _ => None,
                }
            }
        })
    }
}

#[cfg(unix)]
impl Drop for DetourGuard {
    fn drop(&mut self) {
        detour_bypass_off();
    }
}

/// Soft-errors that can be recovered from by calling the raw FFI function.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bypass {
    /// We're dealing with a socket port value that should be ignored, according to the incoming
    /// config.
    IgnoredInIncoming(SocketAddr),

    /// The socket type does not match one of our handled
    /// [`SocketKind`](crate::socket::SocketKind)s.
    Type(i32),

    /// Either an invalid socket domain, or one that we don't handle.
    Domain(i32),

    /// Unix socket to address that was not configured to be connected remotely.
    UnixSocket(Option<String>),

    /// We could not find this [`RawFd`] in neither [`OPEN_FILES`](mirrord_layer::file::OPEN_FILES), nor
    /// [`SOCKETS`](crate::socket::SOCKETS).
    #[cfg(unix)]
    LocalFdNotFound(RawFd),

    /// Similar to `LocalFdNotFound`, but for [`OPEN_DIRS`](mirrord_layer::file::open_dirs::OPEN_DIRS).
    LocalDirStreamNotFound(usize),

    /// A conversion from [`SockAddr`](socket2::SockAddr) to
    /// [`SocketAddr`] failed.
    AddressConversion,

    /// The socket [`RawFd`] is in an invalid state for the operation.
    #[cfg(unix)]
    InvalidState(RawFd),

    /// We got an `Utf8Error` while trying to convert a `CStr` into a safer string type.
    CStrConversion,

    /// We hooked a file operation on a path in mirrord's bin directory. So do the operation
    /// locally, but on the original path, not the one in mirrord's dir.
    #[cfg(target_os = "macos")]
    FileOperationInMirrordBinTempDir(*const c_char),

    /// File [`PathBuf`] should be ignored (used for tests).
    #[cfg(unix)]
    IgnoredFile(CString),

    /// Multiple file [`PathBuf`] that should be ignored.
    ///
    /// Used for functions that must apply `fs.mapping` to multiple files on bypass.
    #[cfg(unix)]
    IgnoredFiles(Option<CString>, Option<CString>),

    /// Some operations only handle absolute [`PathBuf`]s.
    #[cfg(unix)]
    RelativePath(CString),

    /// Started mirrord with [`FsModeConfig`](mirrord_config::feature::fs::mode::FsModeConfig) set
    /// to [`FsModeConfig::Read`](mirrord_config::feature::fs::FsModeConfig::Read), but
    /// operation requires more file permissions.
    ///
    /// The user will reach this case if they started mirrord with file operations as _read-only_,
    /// but tried to perform a file operation that requires _write_ permissions (for example).
    ///
    /// When this happens, the file operation will be bypassed (will be handled locally, instead of
    /// through the agent).
    #[cfg(unix)]
    ReadOnly(PathBuf),

    /// Called [`write`](crate::file::ops::write) with `write_bytes` set to [`None`].
    EmptyBuffer,

    /// Operation received [`None`] for an [`Option`] that was required to be [`Some`].
    EmptyOption,

    /// Called `getaddrinfo` with `rawish_node` being [`None`].
    NullNode,

    /// Skip patching SIP for macOS.
    #[cfg(target_os = "macos")]
    NoSipDetected(String),

    /// Tried patching SIP for a non-existing binary.
    #[cfg(target_os = "macos")]
    ExecOnNonExistingFile(String),

    /// Reached `MAX_ARGC` while running
    /// `intercept_tmp_dir`
    #[cfg(target_os = "macos")]
    TooManyArgs,

    /// Application is binding a port, while mirrord is running targetless. A targetless agent does
    /// is not exposed by a service, so bind locally.
    BindWhenTargetless,

    /// Hooked a `connect` to a target that is disabled in the configuration.
    DisabledOutgoing,

    /// Incoming traffic is disabled, bypass.
    DisabledIncoming,

    /// Hostname should be resolved locally.
    /// Currently, this is the case only when the layer operates in the `trace only` mode.
    LocalHostname,

    /// DNS query should be done locally.
    LocalDns,

    /// Operation is not implemented, but it should not be a hard error.
    ///
    /// Useful for operations that are version gated, and we want to bypass when the protocol
    /// doesn't support them.
    NotImplemented,

    /// File `open` (any `open`-ish operation) was forced to be local, instead of remote, most
    /// likely due to an operator fs policy.
    OpenLocal,
}

#[cfg(unix)]
impl Bypass {
    pub fn relative_path(path: impl Into<Vec<u8>>) -> Self {
        Bypass::RelativePath(CString::new(path).expect("should be a valid C string"))
    }

    pub fn ignored_file(path: impl Into<Vec<u8>>) -> Self {
        Bypass::IgnoredFile(CString::new(path).expect("should be a valid C string"))
    }
}

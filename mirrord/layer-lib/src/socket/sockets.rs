// Unified socket collection for both Unix and Windows layers
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

#[cfg(unix)]
use bincode::{Decode, Encode};
#[cfg(windows)]
use winapi::um::winsock2::SOCKET;

use super::UserSocket;

// Platform-specific socket descriptors
#[cfg(unix)]
pub type SocketDescriptor = i32; // RawFd

#[cfg(windows)]
pub type SocketDescriptor = SOCKET;

/// Environment variable used to share sockets between parent and child processes via exec (Unix
/// only)
pub const SHARED_SOCKETS_ENV_VAR: &str = "MIRRORD_SHARED_SOCKETS";

/// Unified socket collection that can be used by both Unix and Windows layers
/// This replaces the platform-specific SOCKETS collections
///
/// For Unix: includes shared socket initialization from environment variables
/// For Windows: starts with empty collection
pub static SOCKETS: LazyLock<Mutex<HashMap<SocketDescriptor, Arc<UserSocket>>>> =
    LazyLock::new(|| {
        #[cfg(unix)]
        {
            use base64::{Engine, engine::general_purpose::URL_SAFE as BASE64_URL_SAFE};
            use bincode::{Decode, Encode};

            std::env::var(SHARED_SOCKETS_ENV_VAR)
                .ok()
                .and_then(|encoded| {
                    BASE64_URL_SAFE
                        .decode(encoded.into_bytes())
                        .inspect_err(|error| {
                            tracing::warn!(
                                ?error,
                                "failed decoding base64 value from {SHARED_SOCKETS_ENV_VAR}"
                            )
                        })
                        .ok()
                })
                .and_then(|decoded| {
                    bincode::decode_from_slice::<Vec<(i32, UserSocket)>, _>(
                        &decoded,
                        bincode::config::standard(),
                    )
                    .inspect_err(|error| {
                        tracing::warn!(?error, "failed parsing shared sockets env value")
                    })
                    .ok()
                })
                .map(|(fds_and_sockets, _)| {
                    // Note: FD_CLOEXEC filtering needs to be done by the Unix layer
                    // since we don't have access to FN_FCNTL here
                    Mutex::new(HashMap::from_iter(
                        fds_and_sockets
                            .into_iter()
                            .map(|(fd, socket)| (fd, Arc::new(socket))),
                    ))
                })
                .unwrap_or_else(|| Mutex::new(HashMap::new()))
        }

        #[cfg(windows)]
        {
            // Windows doesn't support shared sockets via environment variables
            Mutex::new(HashMap::new())
        }
    });

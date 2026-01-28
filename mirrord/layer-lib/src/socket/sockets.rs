// Unified socket collection for both Unix and Windows layers
#[cfg(unix)]
use std::os::fd::RawFd;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex},
};

use base64::{Engine, engine::general_purpose::URL_SAFE as BASE64_URL_SAFE};
#[cfg(unix)]
use libc;
#[cfg(windows)]
use winapi::um::winsock2::SOCKET;

use super::{SocketKind, SocketState, UserSocket};

// Platform-specific socket descriptors
// RawFd
#[cfg(unix)]
pub type SocketDescriptor = RawFd;

#[cfg(windows)]
pub type SocketDescriptor = SOCKET;

/// Environment variable used to share sockets between parent and child processes
pub const SHARED_SOCKETS_ENV_VAR: &str = "MIRRORD_SHARED_SOCKETS";

/// Stores the [`UserSocket`]s created by the user.
///
/// **Warning**: Do not put logs in here! If you try logging stuff inside this initialization
/// you're gonna have a bad time. The process hanging is the min you should expect, if you
/// choose to ignore this warning.
///
/// - [`SHARED_SOCKETS_ENV_VAR`]: Some sockets may have been initialized by a parent process through
///   [`libc::execve`] (or any `exec*`), and the spawned children may want to use those sockets. As
///   memory is not shared via `exec*` calls (unlike `fork`), we need a way to pass parent sockets
///   to child processes. The way we achieve this is by setting the [`SHARED_SOCKETS_ENV_VAR`] with
///   an [`BASE64_URL_SAFE`] encoded version of our [`SOCKETS`]. The env var is set as
///   `MIRRORD_SHARED_SOCKETS=({fd}, {UserSocket}),*`.
///
/// - [`libc::FD_CLOEXEC`] behaviour: While rebuilding sockets from the env var, we also check if
///   they're set with the cloexec flag, so that children processes don't end up using sockets that
///   are exclusive for their parents.
pub static SOCKETS: LazyLock<Mutex<HashMap<SocketDescriptor, Arc<UserSocket>>>> =
    LazyLock::new(|| {
        std::env::var(SHARED_SOCKETS_ENV_VAR)
            .ok()
            .and_then(|encoded| {
                BASE64_URL_SAFE
                    .decode(encoded.as_bytes())
                    .inspect_err(|error| {
                        tracing::warn!(
                            ?error,
                            "failed decoding base64 value from {SHARED_SOCKETS_ENV_VAR}"
                        )
                    })
                    .ok()
            })
            .and_then(|decoded| {
                bincode::decode_from_slice::<Vec<(SocketDescriptor, UserSocket)>, _>(
                    &decoded,
                    bincode::config::standard(),
                )
                .inspect_err(|error| {
                    tracing::warn!(?error, "failed parsing shared sockets env value")
                })
                .ok()
            })
            .map(|(fds_and_sockets, _)| {
                // Note: filter_map is needed for unix filtering,
                //  on windows it's just a map, shush clippy.
                #[allow(clippy::unnecessary_filter_map)]
                Mutex::new(HashMap::from_iter(fds_and_sockets.into_iter().filter_map(
                    |(fd, socket)| {
                        #[cfg(unix)]
                        {
                            // Do not inherit sockets that are `FD_CLOEXEC`.
                            // NOTE: The original `fcntl` is called instead of `FN_FCNTL` because
                            // the latter may be null at this point,
                            // likely due to child-spawning functions that mess
                            // with memory such as fork/exec.
                            // See: https://github.com/metalbear-co/mirrord-intellij/issues/374
                            if unsafe { libc::fcntl(fd, libc::F_GETFD, 0) } == -1 {
                                return None;
                            }
                        }

                        Some((fd as SocketDescriptor, Arc::new(socket)))
                    },
                )))
            })
            .unwrap_or_default()
    });

/// Helper function to safely convert socket descriptors to i64 for error handling and logging
pub fn socket_descriptor_to_i64(socket: SocketDescriptor) -> i64 {
    socket as i64
}

/// Helper function to convert i64 back to SocketDescriptor (for cases where it's needed)
pub fn i64_to_socket_descriptor(value: i64) -> SocketDescriptor {
    value as SocketDescriptor
}

// Helper function to convert socket types to SocketKind
#[cfg(windows)]
fn socket_kind_from_type(socket_type: i32) -> Result<SocketKind, String> {
    use winapi::um::winsock2::{SOCK_DGRAM, SOCK_STREAM};

    if socket_type == SOCK_STREAM {
        Ok(SocketKind::Tcp(socket_type))
    } else if socket_type == SOCK_DGRAM {
        Ok(SocketKind::Udp(socket_type))
    } else {
        Err(format!("Unsupported socket type: {}", socket_type))
    }
}

#[cfg(unix)]
fn socket_kind_from_type(socket_type: i32) -> Result<SocketKind, String> {
    use libc::{SOCK_DGRAM, SOCK_STREAM};

    if socket_type == SOCK_STREAM {
        Ok(SocketKind::Tcp(socket_type))
    } else if socket_type == SOCK_DGRAM {
        Ok(SocketKind::Udp(socket_type))
    } else {
        Err(format!("Unsupported socket type: {}", socket_type))
    }
}

/// Register a new socket with the unified SOCKETS collection
pub fn register_socket(socket: SocketDescriptor, domain: i32, socket_type: i32, protocol: i32) {
    let kind = match socket_kind_from_type(socket_type) {
        Ok(kind) => kind,
        Err(e) => {
            tracing::warn!("Failed to create socket kind: {}", e);
            return;
        }
    };

    let user_socket = UserSocket::new(
        domain,
        socket_type,
        protocol,
        SocketState::Initialized,
        kind,
    );

    let mut sockets = match SOCKETS.lock() {
        Ok(sockets) => sockets,
        Err(poisoned) => {
            tracing::warn!(
                "SocketManager: sockets mutex was poisoned during registration, attempting recovery"
            );
            poisoned.into_inner()
        }
    };

    sockets.insert(socket, Arc::new(user_socket));
    tracing::info!("SocketManager: Registered socket {} with mirrord", socket);
}

/// Update the state of a managed socket
pub fn set_socket_state(socket: SocketDescriptor, new_state: SocketState) {
    let mut sockets = match SOCKETS.lock() {
        Ok(sockets) => sockets,
        Err(poisoned) => {
            tracing::warn!(
                "SocketManager: sockets mutex was poisoned during state update, attempting recovery"
            );
            poisoned.into_inner()
        }
    };

    if let Some(socket_entry) = sockets.remove(&socket) {
        let mut socket_inner = Arc::try_unwrap(socket_entry)
            .expect("SocketManager: socket Arc unexpectedly shared during state update");
        socket_inner.state = new_state;
        sockets.insert(socket, Arc::new(socket_inner));
        tracing::debug!("SocketManager: Updated socket {} state", socket);
    } else {
        tracing::warn!(
            "SocketManager: Attempted to update state for unmanaged socket {}",
            socket
        );
    }
}

/// Remove a socket from the managed collection
pub fn remove_socket(socket: SocketDescriptor) {
    let mut sockets = match SOCKETS.lock() {
        Ok(sockets) => sockets,
        Err(poisoned) => {
            tracing::warn!(
                "SocketManager: sockets mutex was poisoned during removal, attempting recovery"
            );
            poisoned.into_inner()
        }
    };

    let sockets_removed = sockets.remove(&socket).is_some();

    if sockets_removed {
        tracing::debug!(
            "SocketManager: Removed socket {} from mirrord tracking",
            socket
        );
    }
}

/// Get socket info
pub fn get_socket(socket: SocketDescriptor) -> Option<Arc<UserSocket>> {
    SOCKETS
        .lock()
        .ok()
        .and_then(|sockets| sockets.get(&socket).cloned())
}

/// Check if a socket is managed by mirrord
pub fn is_socket_managed(socket: SocketDescriptor) -> bool {
    SOCKETS
        .lock()
        .map(|sockets| sockets.contains_key(&socket))
        .unwrap_or(false)
}

/// Get socket state for a specific socket
pub fn get_socket_state(socket: SocketDescriptor) -> Option<SocketState> {
    SOCKETS
        .lock()
        .ok()
        .and_then(|sockets| sockets.get(&socket).map(|s| s.state.clone()))
}

/// Check if socket is in a specific state
pub fn is_socket_in_state(
    socket: SocketDescriptor,
    state_check: impl Fn(&SocketState) -> bool,
) -> bool {
    get_socket_state(socket)
        .map(|state| state_check(&state))
        .unwrap_or(false)
}

/// Get bound address for a socket if it's in bound state
///
/// For localhost addresses that were bound to port 0 (let OS choose), returns the actual
/// bound address so that local clients can connect to it. For other addresses, returns
/// the requested address to maintain the mirrord illusion.
pub fn get_bound_address(socket: SocketDescriptor) -> Option<SocketAddr> {
    get_socket_state(socket).and_then(|state| match state {
        SocketState::Bound { bound, .. } | SocketState::Listening(bound) => {
            // For localhost binds to port 0, return the actual bound address
            // so that local clients (like Python's socketpair()) can connect
            if bound.requested_address.ip().is_loopback() && bound.requested_address.port() == 0 {
                Some(bound.address)
            } else {
                Some(bound.requested_address)
            }
        }
        _ => None,
    })
}

/// Find the actual bound address of a listening socket that matches the given port and protocol.
/// Used to detect local self-connections so they can be handled without proxying.
pub fn find_listener_address_by_port(port: u16, protocol: i32) -> Option<SocketAddr> {
    SOCKETS.lock().ok().and_then(|sockets| {
        sockets.iter().find_map(|(_, socket)| match socket.state {
            SocketState::Listening(bound) => {
                if bound.requested_address.port() == port && socket.protocol == protocol {
                    Some(bound.address)
                } else {
                    None
                }
            }
            _ => None,
        })
    })
}

/// Get connected addresses for a socket if it's in connected state
pub fn get_connected_addresses(
    socket: SocketDescriptor,
) -> Option<(
    mirrord_protocol::outgoing::SocketAddress,
    Option<mirrord_protocol::outgoing::SocketAddress>,
    Option<mirrord_protocol::outgoing::SocketAddress>,
)> {
    get_socket_state(socket).and_then(|state| match state {
        SocketState::Connected(connected) => Some((
            connected.remote_address,
            connected.local_address,
            connected.layer_address,
        )),
        _ => None,
    })
}

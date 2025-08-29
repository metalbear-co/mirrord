// Dedicated module for Windows socket state management
use crate::socket::WindowsDnsResolver;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex},
};

use mirrord_intproxy_protocol::{
    NetProtocol, OutgoingConnectRequest, PortSubscribe, PortSubscription,
};
// Re-export shared types from layer-lib
pub use mirrord_layer_lib::socket::{Bound, Connected, SocketKind, SocketState, UserSocket};
use mirrord_protocol::{ConnectionId, outgoing::SocketAddress, tcp::StealType};
use winapi::{
    shared::{minwindef::INT, ws2def::SOCKADDR},
    um::winsock2::SOCKET,
};

use super::{hostname::make_windows_proxy_request_with_response, utils::SocketAddrExtWin};

// Helper function to convert Windows socket types to SocketKind
fn socket_kind_from_type(socket_type: i32) -> Result<SocketKind, String> {
    #[cfg(target_os = "windows")]
    use winapi::um::winsock2::{SOCK_DGRAM, SOCK_STREAM};

    if socket_type == SOCK_STREAM {
        Ok(SocketKind::Tcp(socket_type))
    } else if socket_type == SOCK_DGRAM {
        Ok(SocketKind::Udp(socket_type))
    } else {
        Err(format!("Unsupported socket type: {}", socket_type))
    }
}

/// Managed socket collection that encapsulates all socket operations
pub struct SocketManager {
    sockets: Mutex<HashMap<SOCKET, Arc<UserSocket>>>,
}

impl SocketManager {
    fn new() -> Self {
        Self {
            sockets: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new socket with the manager
    pub fn register_socket(&self, socket: SOCKET, domain: i32, socket_type: i32, protocol: i32) {
        let kind = match socket_kind_from_type(socket_type) {
            Ok(kind) => kind,
            Err(e) => {
                tracing::warn!("Failed to create socket kind: {}", e);
                return;
            }
        };

        let user_socket = Arc::new(UserSocket::new(
            domain,
            socket_type,
            protocol,
            SocketState::Initialized,
            kind,
        ));

        let mut sockets = match self.sockets.lock() {
            Ok(sockets) => sockets,
            Err(poisoned) => {
                tracing::warn!(
                    "SocketManager: sockets mutex was poisoned during registration, attempting recovery"
                );
                poisoned.into_inner()
            }
        };

        sockets.insert(socket, user_socket);
        tracing::info!("SocketManager: Registered socket {} with mirrord", socket);
    }

    /// Update the state of a managed socket
    pub fn set_socket_state(&self, socket: SOCKET, new_state: SocketState) {
        let mut sockets = match self.sockets.lock() {
            Ok(sockets) => sockets,
            Err(poisoned) => {
                tracing::warn!(
                    "SocketManager: sockets mutex was poisoned during state update, attempting recovery"
                );
                poisoned.into_inner()
            }
        };

        if let Some(socket_ref) = sockets.get_mut(&socket) {
            if let Some(socket_mut) = Arc::get_mut(socket_ref) {
                socket_mut.state = new_state;
            } else {
                let mut new_socket = (**socket_ref).clone();
                new_socket.state = new_state;
                sockets.insert(socket, Arc::new(new_socket));
            }
        }
    }

    /// Remove a socket from the managed collection
    pub fn remove_socket(&self, socket: SOCKET) {
        let mut sockets = match self.sockets.lock() {
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
    pub fn get_socket(&self, socket: SOCKET) -> Option<Arc<UserSocket>> {
        self.sockets
            .lock()
            .ok()
            .and_then(|sockets| sockets.get(&socket).cloned())
    }

    /// Check if a socket is managed by mirrord
    pub fn is_socket_managed(&self, socket: SOCKET) -> bool {
        self.sockets
            .lock()
            .map(|sockets| sockets.contains_key(&socket))
            .unwrap_or(false)
    }

    /// Get socket state for a specific socket
    pub fn get_socket_state(&self, socket: SOCKET) -> Option<SocketState> {
        self.sockets
            .lock()
            .ok()
            .and_then(|sockets| sockets.get(&socket).map(|s| s.state.clone()))
    }

    /// Check if socket is in a specific state
    pub fn is_socket_in_state(
        &self,
        socket: SOCKET,
        state_check: impl Fn(&SocketState) -> bool,
    ) -> bool {
        self.get_socket_state(socket)
            .map(|state| state_check(&state))
            .unwrap_or(false)
    }

    /// Get bound address for a socket if it's in bound state
    pub fn get_bound_address(&self, socket: SOCKET) -> Option<SocketAddr> {
        self.get_socket_state(socket).and_then(|state| match state {
            SocketState::Bound(bound) => Some(bound.requested_address),
            SocketState::Listening(bound) => Some(bound.requested_address),
            _ => None,
        })
    }

    /// Get connected addresses for a socket if it's in connected state
    pub fn get_connected_addresses(
        &self,
        socket: SOCKET,
    ) -> Option<(SocketAddress, SocketAddress, Option<SocketAddress>)> {
        self.get_socket_state(socket).and_then(|state| match state {
            SocketState::Connected(connected) => Some((
                connected.remote_address,
                connected.local_address,
                connected.layer_address,
            )),
            _ => None,
        })
    }
}

/// Global socket manager instance
pub static SOCKET_MANAGER: LazyLock<SocketManager> = LazyLock::new(|| SocketManager::new());

/// Result of attempting to establish a proxy connection
#[derive(Debug)]
pub enum ProxyConnectResult {
    Success(Connected, Option<ConnectionId>),
    Fallback,
}

/// Perform a proxy bind operation and return the bound address
pub fn proxy_bind(
    socket: winapi::um::winsock2::SOCKET,
    requested_addr: SocketAddr,
) -> Result<SocketAddr, i32> {
    tracing::info!(
        "proxy_bind -> binding socket {} to address {} through mirrord proxy",
        socket,
        requested_addr
    );

    // For mirrord, we typically bind to the requested address locally first
    // The actual proxy behavior for incoming traffic is handled during listen
    // This creates the bound state that can later transition to listening

    let bound_addr = requested_addr; // In the future, this could be modified by proxy config

    // Create the bound state
    let bound = Bound {
        requested_address: bound_addr,
        address: bound_addr, // For now, use the same address
    };

    // Update socket state to bound using the socket manager
    SOCKET_MANAGER.set_socket_state(socket, SocketState::Bound(bound.clone()));

    tracing::info!(
        "proxy_bind -> socket {} successfully bound to {}",
        socket,
        bound_addr
    );
    Ok(bound_addr)
}

/// Setup listening state for a bound socket and subscribe to incoming connections
pub fn setup_listening(
    socket: winapi::um::winsock2::SOCKET,
    bind_addr: SocketAddr,
    _backlog: i32,
) -> Result<(), String> {
    // Subscribe to port for incoming traffic
    let port_subscribe = PortSubscribe {
        listening_on: bind_addr,
        subscription: PortSubscription::Steal(StealType::All(bind_addr.port())),
    };

    match make_windows_proxy_request_with_response(port_subscribe) {
        Ok(_) => {
            tracing::info!(
                "setup_listening -> successfully subscribed to port {} for socket {}",
                bind_addr.port(),
                socket
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                "setup_listening -> failed to subscribe to port {}: {}",
                bind_addr.port(),
                e
            );
            Err(format!(
                "Failed to subscribe to port {}: {}",
                bind_addr.port(),
                e
            ))
        }
    }
}

/// Register an accepted socket with connection information
pub fn register_accepted_socket(
    socket: winapi::um::winsock2::SOCKET,
    domain: i32,
    socket_type: i32,
    peer_address: SocketAddr,
    local_address: SocketAddr,
) -> Result<(), String> {
    // Create a connected socket state
    let connected = Connected {
        remote_address: SocketAddress::Ip(peer_address),
        local_address: SocketAddress::Ip(local_address),
        layer_address: None,
    };

    // Register the accepted socket using the socket manager
    SOCKET_MANAGER.register_socket(socket, domain, socket_type, 0);
    SOCKET_MANAGER.set_socket_state(socket, SocketState::Connected(connected));

    tracing::info!(
        "register_accepted_socket -> registered socket {} with peer {} and local {}",
        socket,
        peer_address,
        local_address
    );
    Ok(())
}

/// Attempt to establish a connection through the mirrord proxy
pub fn connect_through_proxy(
    _socket: SOCKET,
    user_socket: &UserSocket,
    remote_addr: SocketAddr,
) -> ProxyConnectResult {
    tracing::info!(
        "connect_through_proxy -> intercepting connection to {}",
        remote_addr
    );

    // Create the proxy request
    let protocol = match user_socket.kind {
        SocketKind::Tcp(_) => NetProtocol::Stream,
        SocketKind::Udp(_) => NetProtocol::Datagrams,
    };

    let remote_address = match SocketAddress::try_from(remote_addr) {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!(
                "connect_through_proxy -> failed to convert address: {:?}",
                e
            );
            return ProxyConnectResult::Fallback;
        }
    };

    let request = OutgoingConnectRequest {
        remote_address: remote_address.clone(),
        protocol,
    };

    // Make the proxy request
    match make_windows_proxy_request_with_response(request) {
        Ok(Ok(response)) => {
            tracing::info!(
                "connect_through_proxy -> got proxy response: layer_address={:?}, in_cluster_address={:?}, connection_id={:?}",
                response.layer_address,
                response.in_cluster_address,
                response.connection_id
            );

            let connected = Connected {
                remote_address: response.in_cluster_address,
                local_address: response.layer_address.clone(),
                layer_address: Some(response.layer_address),
            };

            // Return the connection_id from the proxy response
            ProxyConnectResult::Success(connected, Some(response.connection_id))
        }
        Ok(Err(e)) => {
            tracing::debug!("connect_through_proxy -> proxy response error: {:?}", e);
            ProxyConnectResult::Fallback
        }
        Err(e) => {
            tracing::debug!("connect_through_proxy -> proxy request failed: {:?}", e);
            ProxyConnectResult::Fallback
        }
    }
}

/// Handle successful proxy connection result, updating socket state
pub fn handle_connection_success(
    socket: SOCKET,
    connected: Connected,
    function_name: &str,
) -> Result<(SOCKADDR, INT), Box<dyn std::error::Error>> {
    // Convert the layer address to SOCKADDR and connect
    let layer_addr = connected.layer_address.as_ref().unwrap();

    // Convert SocketAddress to SocketAddr
    let socket_addr = match layer_addr {
        SocketAddress::Ip(addr) => *addr,
        _ => return Err("Unix sockets not supported on Windows".into()),
    };

    // Create a buffer for the sockaddr
    let mut sockaddr_buffer: [u8; 128] = [0; 128];
    let mut sockaddr_len = sockaddr_buffer.len() as INT;

    // Use the SocketAddrExtWin trait method
    unsafe {
        socket_addr.to_windows_sockaddr_checked(
            sockaddr_buffer.as_mut_ptr() as *mut SOCKADDR,
            &mut sockaddr_len,
        )?;
    }

    let sockaddr = unsafe { std::ptr::read(sockaddr_buffer.as_ptr() as *const SOCKADDR) };

    // Update socket state first
    SOCKET_MANAGER.set_socket_state(socket, SocketState::Connected(connected));

    tracing::debug!("{} -> updated socket state to Connected", function_name);

    Ok((sockaddr, sockaddr_len))
}

/// Log connection result and return it
pub fn log_connection_result(result: INT, function_name: &str) -> INT {
    if result == 0 {
        tracing::info!(
            "{} -> successfully connected to layer address",
            function_name
        );
    } else {
        tracing::error!(
            "{} -> failed to connect to layer address: {}",
            function_name,
            result
        );
    }
    result
}

/// Result of socket configuration validation
#[derive(Debug)]
pub enum SocketValidationResult {
    /// Socket should be intercepted
    Intercept,
    /// Socket should fall back to original function
    Fallback,
    /// Socket is not managed by mirrord
    NotManaged,
}

/// Check if a socket is managed and if outgoing traffic should be intercepted
pub fn validate_socket_for_outgoing(socket: SOCKET, function_name: &str) -> SocketValidationResult {
    let Some(user_socket) = SOCKET_MANAGER.get_socket(socket) else {
        return SocketValidationResult::NotManaged;
    };

    tracing::debug!(
        "{} -> socket {} is managed, kind: {:?}",
        function_name,
        socket,
        user_socket.kind
    );

    // Check if outgoing traffic is enabled for this socket type
    let should_intercept = match user_socket.kind {
        SocketKind::Tcp(_) => {
            let tcp_outgoing = crate::layer_config().feature.network.outgoing.tcp;
            tracing::info!(
                "{} -> TCP outgoing enabled: {}",
                function_name,
                tcp_outgoing
            );
            tcp_outgoing
        }
        SocketKind::Udp(_) => {
            let udp_outgoing = crate::layer_config().feature.network.outgoing.udp;
            tracing::info!(
                "{} -> UDP outgoing enabled: {}",
                function_name,
                udp_outgoing
            );
            udp_outgoing
        }
    };

    if should_intercept {
        SocketValidationResult::Intercept
    } else {
        tracing::info!(
            "{} -> outgoing traffic disabled for {:?}, falling back to original",
            function_name,
            user_socket.kind
        );
        SocketValidationResult::Fallback
    }
}

/// Result of complete proxy connection attempt including validation and preparation
pub enum ProxyConnectionResult {
    /// Connection was successfully prepared, returns prepared sockaddr and length
    Success((SOCKADDR, INT)),
    /// Connection should fall back to original (not managed, disabled, or failed)
    Fallback,
}

/// Complete proxy connection flow that handles validation, conversion, and preparation
///
/// This function encapsulates the entire flow from raw sockaddr to prepared connection:
/// 1. Validates socket for outgoing traffic interception
/// 2. Converts Windows sockaddr to Rust SocketAddr
/// 3. Attempts proxy connection through mirrord
/// 4. Handles connection success and prepares final sockaddr
///
/// Returns either a prepared sockaddr ready for the original connect function,
/// or Fallback to indicate the caller should use the original function.
pub fn attempt_proxy_connection(
    socket: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    function_name: &str,
) -> ProxyConnectionResult {
    use super::utils::SocketAddrExtWin;

    // Validate socket and check configuration
    match validate_socket_for_outgoing(socket, function_name) {
        SocketValidationResult::Intercept => {
            // Get the socket state (we know it exists from validation)
            let user_socket = match SOCKET_MANAGER.get_socket(socket) {
                Some(socket) => socket,
                None => {
                    tracing::error!(
                        "{} -> socket {} validated but not found in manager",
                        function_name,
                        socket
                    );
                    return ProxyConnectionResult::Fallback;
                }
            };

            // Convert Windows sockaddr to Rust SocketAddr
            let remote_addr = match SocketAddr::try_from_raw(name, namelen) {
                Some(addr) => addr,
                None => {
                    tracing::warn!(
                        "{} -> failed to convert sockaddr, falling back to original",
                        function_name
                    );
                    return ProxyConnectionResult::Fallback;
                }
            };

            // Determine the protocol based on the socket type
            let protocol = match user_socket.kind {
                SocketKind::Tcp(_) => NetProtocol::Stream,
                SocketKind::Udp(_) => NetProtocol::Datagrams,
            };

            // Check the outgoing selector to determine routing
            let resolver = WindowsDnsResolver;
            match crate::layer_setup()
                .outgoing_selector
                .get_connection_through_with_resolver(remote_addr, protocol, &resolver)
            {
                Ok(crate::setup::ConnectionThrough::Remote(filtered_addr)) => {
                    tracing::debug!(
                        "{} -> outgoing filter indicates remote connection for {:?} -> {:?}",
                        function_name,
                        remote_addr,
                        filtered_addr
                    );
                    // Continue with proxy connection using the filtered address
                }
                Ok(crate::setup::ConnectionThrough::Local(_)) => {
                    tracing::debug!(
                        "{} -> outgoing filter indicates local connection for {:?}, falling back to original",
                        function_name,
                        remote_addr
                    );
                    return ProxyConnectionResult::Fallback;
                }
                Err(e) => {
                    tracing::warn!(
                        "{} -> outgoing filter check failed: {}, falling back to original",
                        function_name,
                        e
                    );
                    return ProxyConnectionResult::Fallback;
                }
            }

            // Try to connect through the mirrord proxy
            match connect_through_proxy(socket, &*user_socket, remote_addr) {
                ProxyConnectResult::Success(connected, _connection_id) => {
                    // Handle connection success and prepare sockaddr
                    match handle_connection_success(socket, connected, function_name) {
                        Ok((sockaddr, sockaddr_len)) => {
                            tracing::debug!(
                                "{} -> proxy connection successful, prepared sockaddr",
                                function_name
                            );
                            ProxyConnectionResult::Success((sockaddr, sockaddr_len))
                        }
                        Err(e) => {
                            tracing::warn!(
                                "{} -> failed to handle connection success: {}, falling back to original",
                                function_name,
                                e
                            );
                            ProxyConnectionResult::Fallback
                        }
                    }
                }
                ProxyConnectResult::Fallback => {
                    tracing::debug!(
                        "{} -> proxy connect failed, falling back to original",
                        function_name
                    );
                    ProxyConnectionResult::Fallback
                }
            }
        }
        SocketValidationResult::NotManaged | SocketValidationResult::Fallback => {
            tracing::debug!(
                "{} -> socket not managed or fallback mode, using original",
                function_name
            );
            ProxyConnectionResult::Fallback
        }
    }
}

// Dedicated module for Windows socket state management
use std::{net::SocketAddr, sync::Arc};

use mirrord_intproxy_protocol::{
    NetProtocol, OutgoingConnectRequest, PortSubscribe, PortSubscription,
};
// Import the new connect_outgoing function from layer-lib
use mirrord_layer_lib::socket::ops::{ConnectError, ConnectResult, connect_outgoing};
// Re-export shared types from layer-lib and use unified SOCKETS
pub use mirrord_layer_lib::socket::{
    Bound, Connected, ConnectionThrough, SOCKETS, SocketKind, SocketState, UserSocket,
};
use mirrord_protocol::{ConnectionId, outgoing::SocketAddress, tcp::StealType};
use socket2::SockAddr;
use winapi::{
    shared::{minwindef::INT, ws2def::SOCKADDR},
    um::winsock2::SOCKET,
};

use super::{hostname::make_windows_proxy_request_with_response, utils::SocketAddrExtWin};
use crate::socket::WindowsDnsResolver;

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

/// Register a new socket with the unified SOCKETS collection
pub fn register_socket(socket: SOCKET, domain: i32, socket_type: i32, protocol: i32) {
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
pub fn set_socket_state(socket: SOCKET, new_state: SocketState) {
    let mut sockets = match SOCKETS.lock() {
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
            // Exclusive access - can modify in place
            socket_mut.state = new_state;
            tracing::debug!("SocketManager: Updated socket {} state in-place", socket);
        } else {
            // Arc is shared - must clone and replace
            let mut new_socket = (**socket_ref).clone();
            new_socket.state = new_state;
            sockets.insert(socket, Arc::new(new_socket));
            tracing::debug!(
                "SocketManager: Arc for socket {} is shared, cloning for state update",
                socket
            );
        }
    } else {
        tracing::warn!(
            "SocketManager: Attempted to update state for unmanaged socket {}",
            socket
        );
    }
}

/// Remove a socket from the managed collection
pub fn remove_socket(socket: SOCKET) {
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
pub fn get_socket(socket: SOCKET) -> Option<Arc<UserSocket>> {
    SOCKETS
        .lock()
        .ok()
        .and_then(|sockets| sockets.get(&socket).cloned())
}

/// Check if a socket is managed by mirrord
pub fn is_socket_managed(socket: SOCKET) -> bool {
    SOCKETS
        .lock()
        .map(|sockets| sockets.contains_key(&socket))
        .unwrap_or(false)
}

/// Get socket state for a specific socket
pub fn get_socket_state(socket: SOCKET) -> Option<SocketState> {
    SOCKETS
        .lock()
        .ok()
        .and_then(|sockets| sockets.get(&socket).map(|s| s.state.clone()))
}

/// Check if socket is in a specific state
pub fn is_socket_in_state(socket: SOCKET, state_check: impl Fn(&SocketState) -> bool) -> bool {
    get_socket_state(socket)
        .map(|state| state_check(&state))
        .unwrap_or(false)
}

/// Get bound address for a socket if it's in bound state
pub fn get_bound_address(socket: SOCKET) -> Option<SocketAddr> {
    get_socket_state(socket).and_then(|state| match state {
        SocketState::Bound(bound) => Some(bound.requested_address),
        SocketState::Listening(bound) => Some(bound.requested_address),
        _ => None,
    })
}

/// Get connected addresses for a socket if it's in connected state
pub fn get_connected_addresses(
    socket: SOCKET,
) -> Option<(SocketAddress, SocketAddress, Option<SocketAddress>)> {
    get_socket_state(socket).and_then(|state| match state {
        SocketState::Connected(connected) => Some((
            connected.remote_address,
            connected.local_address,
            connected.layer_address,
        )),
        _ => None,
    })
}

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

    // Update socket state to bound using the unified SOCKETS
    set_socket_state(socket, SocketState::Bound(bound.clone()));

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

    // Register the accepted socket using the unified SOCKETS
    register_socket(socket, domain, socket_type, 0);
    set_socket_state(socket, SocketState::Connected(connected));

    tracing::info!(
        "register_accepted_socket -> registered socket {} with peer {} and local {}",
        socket,
        peer_address,
        local_address
    );
    Ok(())
}

/// Attempt to establish a connection through the mirrord proxy using layer-lib
/// This integrates with the shared connect_outgoing logic from layer-lib
pub fn connect_through_proxy_with_layer_lib<F>(
    socket: SOCKET,
    user_socket: Arc<UserSocket>,
    remote_addr: SocketAddr,
    connect_fn: F,
) -> Result<ConnectResult, ConnectError>
where
    F: FnOnce(SOCKET, SockAddr) -> ConnectResult,
{
    tracing::info!(
        "connect_through_proxy_with_layer_lib -> intercepting connection to {}",
        remote_addr
    );

    // Create the proxy request function that matches layer-lib expectations
    let proxy_request_fn = |request: OutgoingConnectRequest| -> Result<_, ConnectError> {
        match make_windows_proxy_request_with_response(request) {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(ConnectError::ProxyRequest(format!("{:?}", e))),
            Err(e) => Err(ConnectError::ProxyRequest(format!("{:?}", e))),
        }
    };

    // Determine the protocol
    let protocol = match user_socket.kind {
        SocketKind::Tcp(_) => NetProtocol::Stream,
        SocketKind::Udp(_) => NetProtocol::Datagrams,
    };

    // Convert SocketAddr to SockAddr for layer-lib
    let remote_sock_addr = SockAddr::from(remote_addr);

    // Use layer-lib's connect_outgoing_with_call function for TCP connections
    connect_outgoing(
        socket,
        remote_sock_addr,
        user_socket.clone(),
        protocol,
        proxy_request_fn,
        connect_fn,
    )
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
    set_socket_state(socket, SocketState::Connected(connected));

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
    let Some(user_socket) = get_socket(socket) else {
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
            let tcp_outgoing = crate::layer_setup().outgoing_config().tcp;
            tracing::info!(
                "{} -> TCP outgoing enabled: {}",
                function_name,
                tcp_outgoing
            );
            tcp_outgoing
        }
        SocketKind::Udp(_) => {
            let udp_outgoing = crate::layer_setup().outgoing_config().udp;
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
pub fn attempt_proxy_connection<F>(
    socket: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    function_name: &str,
    connect_fn: F,
) -> Result<ConnectResult, ConnectError>
where
    F: FnOnce(SOCKET, SockAddr) -> ConnectResult,
{
    use super::utils::SocketAddrExtWin;

    // Validate socket and check configuration
    match validate_socket_for_outgoing(socket, function_name) {
        SocketValidationResult::NotManaged | SocketValidationResult::Fallback => {
            tracing::debug!(
                "{} -> socket not managed or fallback mode, using original",
                function_name
            );
            return Err(ConnectError::Fallback);
        }
        SocketValidationResult::Intercept => {
            // proceed
        }
    }

    // Get the socket state (we know it exists from validation)
    let user_socket = match get_socket(socket) {
        Some(socket) => socket,
        None => {
            tracing::error!(
                "{} -> socket {} validated but not found in manager",
                function_name,
                socket
            );
            return Err(ConnectError::Fallback);
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
            return Err(ConnectError::Fallback);
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
        .outgoing_selector()
        .get_connection_through_with_resolver(remote_addr, protocol, &resolver)
    {
        Ok(ConnectionThrough::Remote(filtered_addr)) => {
            tracing::debug!(
                "{} -> outgoing filter indicates remote connection for {:?} -> {:?}",
                function_name,
                remote_addr,
                filtered_addr
            );
            // Continue with proxy connection using the filtered address
        }
        Ok(ConnectionThrough::Local(_)) => {
            tracing::debug!(
                "{} -> outgoing filter indicates local connection for {:?}, falling back to original",
                function_name,
                remote_addr
            );
            return Err(ConnectError::Fallback);
        }
        Err(e) => {
            tracing::warn!(
                "{} -> outgoing filter check failed: {}, falling back to original",
                function_name,
                e
            );
            return Err(ConnectError::Fallback);
        }
    }

    // Try to connect through the mirrord proxy using layer-lib integration
    connect_through_proxy_with_layer_lib(socket, user_socket, remote_addr, connect_fn)
}

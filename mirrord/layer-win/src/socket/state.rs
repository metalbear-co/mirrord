// Dedicated module for Windows socket state management
use std::{net::SocketAddr, sync::Arc};

use mirrord_intproxy_protocol::{
    NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse, PortSubscribe, PortSubscription,
};
// Import the new connect_outgoing function from layer-lib
use mirrord_layer_lib::ConnectError;
use mirrord_layer_lib::socket::ops::{ConnectResult, connect_outgoing};
// Re-export shared types from layer-lib and use unified SOCKETS
pub use mirrord_layer_lib::socket::{
    Bound, Connected, ConnectionThrough, HookResult, SocketDescriptor, SocketKind, SocketState,
    UserSocket, get_socket, is_ignored_port, register_socket, set_socket_state,
};
use mirrord_protocol::{outgoing::SocketAddress, tcp::StealType};
use socket2::SockAddr;
use winapi::{
    shared::{minwindef::INT, ws2def::SOCKADDR},
    um::winsock2::SOCKET,
};

use super::hostname::make_windows_proxy_request_with_response;
use crate::socket::{WindowsDnsResolver, hostname};

/// Windows-specific wrapper for registering sockets with additional Windows context
pub fn register_windows_socket(socket: SOCKET, domain: i32, socket_type: i32, protocol: i32) {
    register_socket(socket, domain, socket_type, protocol);
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
    register_windows_socket(socket, domain, socket_type, 0);
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
) -> HookResult<ConnectResult>
where
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    tracing::info!(
        "connect_through_proxy_with_layer_lib -> intercepting connection to {}",
        remote_addr
    );

    let raw_remote_addr = SockAddr::from(remote_addr);
    let optional_ip_address = raw_remote_addr.as_socket();
    if let Some(ip_address) = optional_ip_address {
        // let ip = ip_address.ip();
        // if (ip.is_loopback() || ip.is_unspecified())
        //     && let Some(result) = connect_to_local_address(sockfd, &user_socket_info,
        // ip_address)? {
        //     // `result` here is always a success, as error and bypass are returned on the `?`
        //     // above.
        //     return Detour::Success(result);
        // }

        if is_ignored_port(&ip_address) {
            return Err(ConnectError::BypassPort(ip_address.port()).into());
        }

        // // Ports 50000 and 50001 are commonly used to communicate with sidecar containers.
        // let bypass_debugger_check = ip_address.port() == 50000 || ip_address.port() == 50001;
        // if bypass_debugger_check.not() && crate::setup().is_debugger_port(&ip_address) {
        //     return Detour::Bypass(Bypass::Port(ip_address.port()));
        // }
    };

    // Create the proxy request function that matches layer-lib expectations
    let proxy_request_fn =
        |request: OutgoingConnectRequest| -> HookResult<OutgoingConnectResponse> {
            match hostname::make_windows_proxy_request_with_response(request) {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(e)) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
                Err(e) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
            }
        };

    // Determine the protocol
    let protocol = match user_socket.kind {
        SocketKind::Tcp(_) => NetProtocol::Stream,
        SocketKind::Udp(_) => NetProtocol::Datagrams,
    };

    // Convert SocketAddr to SockAddr for layer-lib
    let remote_sock_addr = SockAddr::from(remote_addr);

    let enabled_tcp_outgoing = crate::layer_setup().outgoing_config().tcp;
    let enabled_udp_outgoing = crate::layer_setup().outgoing_config().udp;

    match NetProtocol::from(user_socket.kind) {
        NetProtocol::Datagrams if enabled_udp_outgoing => connect_outgoing::<true, _, _>(
            socket,
            remote_sock_addr,
            user_socket,
            NetProtocol::Datagrams,
            proxy_request_fn,
            connect_fn,
        ),

        NetProtocol::Stream => match user_socket.state {
            SocketState::Initialized | SocketState::Bound(..)
                // || (remote_addr.is_unix() && !unix_streams.is_empty())
                if (optional_ip_address.is_some() && enabled_tcp_outgoing) => connect_outgoing::<true, _, _>(
                    socket,
                    remote_sock_addr,
                    user_socket,
                    protocol,
                    proxy_request_fn,
                    connect_fn,
                ),

            _ => Err(ConnectError::DisabledOutgoing(socket).into())
        },

        _ => Err(ConnectError::DisabledOutgoing(socket).into()),
    }
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
) -> HookResult<ConnectResult>
where
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    use super::utils::SocketAddrExtWin;

    // Validate socket and check configuration
    match validate_socket_for_outgoing(socket, function_name) {
        SocketValidationResult::NotManaged | SocketValidationResult::Fallback => {
            tracing::debug!(
                "{} -> socket not managed or fallback mode, using original",
                function_name
            );
            return Err(ConnectError::Fallback.into());
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
            return Err(ConnectError::Fallback.into());
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
            return Err(ConnectError::Fallback.into());
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
            return Err(ConnectError::Fallback.into());
        }
        Err(e) => {
            tracing::warn!(
                "{} -> outgoing filter check failed: {}, falling back to original",
                function_name,
                e
            );
            return Err(ConnectError::Fallback.into());
        }
    }

    // Try to connect through the mirrord proxy using layer-lib integration
    connect_through_proxy_with_layer_lib(socket, user_socket, remote_addr, connect_fn)
}

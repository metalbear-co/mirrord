// Dedicated module for Windows socket state management
use std::net::SocketAddr;

use mirrord_intproxy_protocol::{PortSubscribe, PortSubscription};
// Re-export shared types from layer-lib and use unified SOCKETS
pub use mirrord_layer_lib::socket::{
    Bound, Connected, SocketState, register_socket, set_socket_state,
};
use mirrord_protocol::{outgoing::SocketAddress, tcp::StealType};
use winapi::um::winsock2::SOCKET;

use super::hostname::make_windows_proxy_request_with_response;

/// Windows-specific wrapper for registering sockets with additional Windows context
pub fn register_windows_socket(socket: SOCKET, domain: i32, socket_type: i32, protocol: i32) {
    register_socket(socket, domain, socket_type, protocol);
}

/// Perform a proxy bind operation and return the bound address
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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

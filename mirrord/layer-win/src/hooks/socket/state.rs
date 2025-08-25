// Dedicated module for Windows socket state management
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex},
};
use mirrord_protocol::{ConnectionId};
use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnectRequest, PortSubscribe, PortSubscription};
use mirrord_protocol::{outgoing::SocketAddress, tcp::StealType};
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{SOCKADDR, SOCKADDR_IN, AF_INET},
    },
    um::winsock2::SOCKET,
};
use crate::hooks::socket::hostname::make_windows_proxy_request_with_response;

#[derive(Debug, Clone)]
pub enum WindowsSocketState {
    Initialized,
    Bound(WindowsSocketBound),
    Listening(WindowsSocketBound),
    Connected(WindowsSocketConnected),
}

#[derive(Debug, Clone)]
pub struct WindowsSocketBound {
    pub address: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct WindowsSocketConnected {
    pub remote_address: mirrord_protocol::outgoing::SocketAddress,
    pub local_address: mirrord_protocol::outgoing::SocketAddress,
    pub layer_address: Option<mirrord_protocol::outgoing::SocketAddress>,
}

#[derive(Debug, Clone)]
pub enum WindowsSocketKind {
    Tcp,
    Udp,
}

#[derive(Debug, Clone)]
pub struct WindowsUserSocket {
    pub domain: i32,
    pub kind: WindowsSocketKind,
    pub state: WindowsSocketState,
}

/// Managed socket collection that encapsulates all socket operations
pub struct SocketManager {
    sockets: Mutex<HashMap<SOCKET, Arc<WindowsUserSocket>>>,
    connection_ids: Mutex<HashMap<SOCKET, ConnectionId>>,
}

impl SocketManager {
    fn new() -> Self {
        Self {
            sockets: Mutex::new(HashMap::new()),
            connection_ids: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new socket with the manager
    pub fn register_socket(&self, socket: SOCKET, domain: i32, kind: WindowsSocketKind) {
        let user_socket = Arc::new(WindowsUserSocket {
            domain,
            kind,
            state: WindowsSocketState::Initialized,
        });
        if let Ok(mut sockets) = self.sockets.lock() {
            sockets.insert(socket, user_socket);
            tracing::info!("SocketManager: Registered socket {} with mirrord", socket);
        }
    }

    /// Update the state of a managed socket
    pub fn set_socket_state(&self, socket: SOCKET, new_state: WindowsSocketState) {
        if let Ok(mut sockets) = self.sockets.lock() {
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
    }

    /// Remove a socket from the managed collection
    pub fn remove_socket(&self, socket: SOCKET) {
        if let Ok(mut sockets) = self.sockets.lock() {
            sockets.remove(&socket);
        }
        if let Ok(mut ids) = self.connection_ids.lock() {
            ids.remove(&socket);
        }
    }

    /// Get socket info
    pub fn get_socket(&self, socket: SOCKET) -> Option<Arc<WindowsUserSocket>> {
        self.sockets.lock().ok().and_then(|sockets| sockets.get(&socket).cloned())
    }

    /// Check if a socket is managed by mirrord
    pub fn is_socket_managed(&self, socket: SOCKET) -> bool {
        self.sockets.lock()
            .map(|sockets| sockets.contains_key(&socket))
            .unwrap_or(false)
    }

    /// Get socket state for a specific socket
    pub fn get_socket_state(&self, socket: SOCKET) -> Option<WindowsSocketState> {
        self.sockets.lock()
            .ok()
            .and_then(|sockets| sockets.get(&socket).map(|s| s.state.clone()))
    }

    /// Check if socket is in a specific state
    pub fn is_socket_in_state(&self, socket: SOCKET, state_check: impl Fn(&WindowsSocketState) -> bool) -> bool {
        self.get_socket_state(socket)
            .map(|state| state_check(&state))
            .unwrap_or(false)
    }

    /// Get bound address for a socket if it's in bound state
    pub fn get_bound_address(&self, socket: SOCKET) -> Option<SocketAddr> {
        self.get_socket_state(socket).and_then(|state| {
            match state {
                WindowsSocketState::Bound(bound) => Some(bound.address),
                WindowsSocketState::Listening(bound) => Some(bound.address),
                _ => None,
            }
        })
    }

    /// Get connected addresses for a socket if it's in connected state
    pub fn get_connected_addresses(&self, socket: SOCKET) -> Option<(SocketAddress, SocketAddress, Option<SocketAddress>)> {
        self.get_socket_state(socket).and_then(|state| {
            match state {
                WindowsSocketState::Connected(connected) => {
                    Some((connected.remote_address, connected.local_address, connected.layer_address))
                }
                _ => None,
            }
        })
    }

    /// Set connection ID for a socket
    pub fn set_connection_id(&self, socket: SOCKET, conn_id: ConnectionId) {
        if let Ok(mut ids) = self.connection_ids.lock() {
            ids.insert(socket, conn_id);
        }
    }

    /// Remove connection ID for a socket
    pub fn remove_connection_id(&self, socket: SOCKET) {
        if let Ok(mut ids) = self.connection_ids.lock() {
            ids.remove(&socket);
        }
    }

    /// Get connection ID for a socket
    pub fn get_connection_id(&self, socket: SOCKET) -> Option<ConnectionId> {
        self.connection_ids.lock().ok().and_then(|ids| ids.get(&socket).cloned())
    }
}

/// Global socket manager instance
pub static SOCKET_MANAGER: LazyLock<SocketManager> = LazyLock::new(|| SocketManager::new());

/// Result of attempting to establish a proxy connection
#[derive(Debug)]
pub enum ProxyConnectResult {
    Success(WindowsSocketConnected, Option<ConnectionId>),
    Fallback,
}

/// Convert SocketAddr to Windows SOCKADDR for address return functions
pub unsafe fn socketaddr_to_windows_sockaddr(addr: &SocketAddr, name: *mut SOCKADDR, namelen: *mut INT) -> Result<(), i32> {
    if name.is_null() || namelen.is_null() {
        return Err(10014); // WSAEFAULT
    }
    
    match addr {
        SocketAddr::V4(addr_v4) => {
            let size = std::mem::size_of::<SOCKADDR_IN>() as INT;
            if unsafe { *namelen } < size {
                return Err(10014); // WSAEFAULT - buffer too small
            }
            
            let mut sockaddr_in: SOCKADDR_IN = unsafe { std::mem::zeroed() };
            sockaddr_in.sin_family = AF_INET as u16;
            sockaddr_in.sin_port = addr_v4.port().to_be();
            unsafe {
                *sockaddr_in.sin_addr.S_un.S_addr_mut() = u32::from(*addr_v4.ip()).to_be();
            }
            
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &sockaddr_in as *const _ as *const u8,
                    name as *mut u8,
                    size as usize,
                );
                *namelen = size;
            }
            
            Ok(())
        }
        SocketAddr::V6(_) => {
            Err(10022) // WSAEINVAL - IPv6 not yet supported
        }
    }
}

/// Perform a proxy bind operation and return the bound address
pub fn proxy_bind(socket: winapi::um::winsock2::SOCKET, requested_addr: SocketAddr) -> Result<SocketAddr, i32> {
    tracing::info!("proxy_bind -> binding socket {} to address {} through mirrord proxy", socket, requested_addr);
    
    // For mirrord, we typically bind to the requested address locally first
    // The actual proxy behavior for incoming traffic is handled during listen
    // This creates the bound state that can later transition to listening
    
    let bound_addr = requested_addr; // In the future, this could be modified by proxy config
    
    // Create the bound state  
    let bound = WindowsSocketBound {
        address: bound_addr,
    };
    
    // Update socket state to bound using the socket manager
    SOCKET_MANAGER.set_socket_state(socket, WindowsSocketState::Bound(bound.clone()));
    
    tracing::info!("proxy_bind -> socket {} successfully bound to {}", socket, bound_addr);
    Ok(bound_addr)
}

/// Setup listening state for a bound socket and subscribe to incoming connections
pub fn setup_listening(socket: winapi::um::winsock2::SOCKET, bind_addr: SocketAddr, _backlog: i32) -> Result<(), String> {
    // Subscribe to port for incoming traffic
    let port_subscribe = PortSubscribe {
        listening_on: bind_addr,
        subscription: PortSubscription::Steal(StealType::All(bind_addr.port())),
    };
    
    match make_windows_proxy_request_with_response(port_subscribe) {
        Ok(_) => {
            tracing::info!("setup_listening -> successfully subscribed to port {} for socket {}", 
                         bind_addr.port(), socket);
            Ok(())
        }
        Err(e) => {
            tracing::error!("setup_listening -> failed to subscribe to port {}: {}", bind_addr.port(), e);
            Err(format!("Failed to subscribe to port {}: {}", bind_addr.port(), e))
        }
    }
}

/// Register an accepted socket with connection information
pub fn register_accepted_socket(socket: winapi::um::winsock2::SOCKET, domain: i32, 
                                kind: WindowsSocketKind, peer_address: SocketAddr, 
                                local_address: SocketAddr) -> Result<(), String> {
    // Create a connected socket state
    let connected = WindowsSocketConnected {
        remote_address: SocketAddress::try_from(peer_address)
            .map_err(|e| format!("Failed to convert peer address: {}", e))?,
        local_address: SocketAddress::try_from(local_address)
            .map_err(|e| format!("Failed to convert local address: {}", e))?,
        layer_address: None,
    };
    
    // Register the accepted socket using the socket manager
    SOCKET_MANAGER.register_socket(socket, domain, kind);
    SOCKET_MANAGER.set_socket_state(socket, WindowsSocketState::Connected(connected));
    
    tracing::info!("register_accepted_socket -> registered socket {} with peer {} and local {}", 
                   socket, peer_address, local_address);
    Ok(())
}

/// Attempt to establish a connection through the mirrord proxy
pub fn connect_through_proxy(
    _socket: SOCKET,
    user_socket: &WindowsUserSocket,
    remote_addr: SocketAddr,
) -> ProxyConnectResult {
    tracing::info!("connect_through_proxy -> intercepting connection to {}", remote_addr);
    
    // Create the proxy request
    let protocol = match user_socket.kind {
        WindowsSocketKind::Tcp => NetProtocol::Stream,
        WindowsSocketKind::Udp => NetProtocol::Datagrams,
    };
    
    let remote_address = match SocketAddress::try_from(remote_addr) {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("connect_through_proxy -> failed to convert address: {:?}", e);
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
            tracing::info!("connect_through_proxy -> got proxy response: layer_address={:?}, in_cluster_address={:?}, connection_id={:?}", 
                response.layer_address, response.in_cluster_address, response.connection_id);
            
            let connected = WindowsSocketConnected {
                remote_address: response.in_cluster_address.clone(),
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
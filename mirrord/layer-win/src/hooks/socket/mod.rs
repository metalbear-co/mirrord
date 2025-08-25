//! Module responsible for registering hooks targeting socket operation syscalls.

mod hostname;

use std::{
    collections::HashMap,
    mem,
    net::SocketAddr,
    ptr,
    sync::{Arc, LazyLock, Mutex, OnceLock},
};

use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnectRequest, PortSubscribe, PortSubscription};
use mirrord_protocol::{
    tcp::StealType,
};
use mirrord_layer_lib::{HostnameResult, get_or_init_hostname, DefaultHostnameResolver};
use mirrord_protocol::outgoing::SocketAddress;
use minhook_detours_rs::guard::DetourGuard;
use winapi::{
    shared::{
        minwindef::{INT},
        ws2def::{SOCKADDR, SOCKADDR_IN, AF_INET, AF_INET6, ADDRINFOA},
        ws2ipdef::{SOCKADDR_IN6},
    },
    um::{
        winsock2::{
            SOCKET, INVALID_SOCKET, SOCK_STREAM, SOCK_DGRAM, HOSTENT, 
        },
    },
};

use crate::apply_hook;
use self::hostname::{
    REMOTE_DNS_REVERSE_MAPPING, evict_old_dns_entries, 
    extract_ip_from_hostent,
    handle_hostname_ansi, handle_hostname_unicode,
    is_remote_hostname, resolve_hostname_with_fallback,
    make_windows_proxy_request_with_response,
    windows_getaddrinfo, free_managed_addrinfo, MANAGED_ADDRINFO
};

/// Windows socket state management
#[derive(Debug, Clone)]
pub enum WindowsSocketState {
    Initialized,
    Bound(WindowsSocketBound),
    Listening(WindowsSocketBound),
    Connected(WindowsSocketConnected),
}

#[derive(Debug, Clone)]
pub struct WindowsSocketBound {
    pub requested_address: SocketAddr,
    pub address: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct WindowsSocketConnected {
    pub remote_address: SocketAddress,
    pub local_address: SocketAddress,
    pub layer_address: Option<SocketAddress>,
}

#[derive(Debug, Clone)]
pub enum WindowsSocketKind {
    Tcp,
    Udp,
}

/// Information about a Windows socket
#[derive(Debug, Clone)]
pub struct WindowsUserSocket {
    pub domain: INT,
    pub kind: WindowsSocketKind,
    pub state: WindowsSocketState,
}

/// Global collection of Windows sockets managed by mirrord
static WINDOWS_SOCKETS: LazyLock<Mutex<HashMap<SOCKET, Arc<WindowsUserSocket>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Helper function to convert Windows SOCKADDR to Rust SocketAddr
unsafe fn sockaddr_to_socket_addr(addr: *const SOCKADDR, addrlen: INT) -> Option<SocketAddr> {
    if addr.is_null() || addrlen < mem::size_of::<SOCKADDR_IN>() as INT {
        return None;
    }
    
    let sa_family = unsafe { (*addr).sa_family };
    match sa_family as i32 {
        AF_INET => {
            if addrlen < mem::size_of::<SOCKADDR_IN>() as INT {
                return None;
            }
            let addr_in = unsafe { &*(addr as *const SOCKADDR_IN) };
            let ip = unsafe { std::net::Ipv4Addr::from(u32::from_be(*addr_in.sin_addr.S_un.S_addr())) };
            let port = u16::from_be(addr_in.sin_port);
            Some(SocketAddr::new(ip.into(), port))
        }
        AF_INET6 => {
            if addrlen < mem::size_of::<SOCKADDR_IN6>() as INT {
                return None;
            }
            let addr_in6 = unsafe { &*(addr as *const SOCKADDR_IN6) };
            let ip = unsafe { std::net::Ipv6Addr::from(*addr_in6.sin6_addr.u.Byte()) };
            let port = u16::from_be(addr_in6.sin6_port);
            Some(SocketAddr::new(ip.into(), port))
        }
        _ => None,
    }
}

// Function type definitions for original Windows socket functions
type SocketFn = unsafe extern "system" fn(af: INT, r#type: INT, protocol: INT) -> SOCKET;
static SOCKET_ORIGINAL: OnceLock<&SocketFn> = OnceLock::new();

type BindFn = unsafe extern "system" fn(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT;
static BIND_ORIGINAL: OnceLock<&BindFn> = OnceLock::new();

type ListenFn = unsafe extern "system" fn(s: SOCKET, backlog: INT) -> INT;
static LISTEN_ORIGINAL: OnceLock<&ListenFn> = OnceLock::new();

type ConnectFn = unsafe extern "system" fn(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT;
static CONNECT_ORIGINAL: OnceLock<&ConnectFn> = OnceLock::new();

type AcceptFn = unsafe extern "system" fn(s: SOCKET, addr: *mut SOCKADDR, addrlen: *mut INT) -> SOCKET;
static ACCEPT_ORIGINAL: OnceLock<&AcceptFn> = OnceLock::new();

type GetsocknameFn = unsafe extern "system" fn(s: SOCKET, name: *mut SOCKADDR, namelen: *mut INT) -> INT;
static GETSOCKNAME_ORIGINAL: OnceLock<&GetsocknameFn> = OnceLock::new();

type GetpeernameFn = unsafe extern "system" fn(s: SOCKET, name: *mut SOCKADDR, namelen: *mut INT) -> INT;
static GETPEERNAME_ORIGINAL: OnceLock<&GetpeernameFn> = OnceLock::new();

type GethostnameFn = unsafe extern "system" fn(name: *mut i8, namelen: INT) -> INT;
static GETHOSTNAME_ORIGINAL: OnceLock<&GethostnameFn> = OnceLock::new();

// DNS resolution functions that Python socket.gethostbyname uses
type GethostbynameFn = unsafe extern "system" fn(name: *const i8) -> *mut HOSTENT;
static GETHOSTBYNAME_ORIGINAL: OnceLock<&GethostbynameFn> = OnceLock::new();

type GetaddrinfoFn = unsafe extern "system" fn(
    node_name: *const i8,
    service_name: *const i8, 
    hints: *const ADDRINFOA,
    result: *mut *mut ADDRINFOA
) -> INT;
static GETADDRINFO_ORIGINAL: OnceLock<&GetaddrinfoFn> = OnceLock::new();

type FreeaddrinfoFn = unsafe extern "system" fn(addrinfo: *mut ADDRINFOA);
static FREEADDRINFO_ORIGINAL: OnceLock<&FreeaddrinfoFn> = OnceLock::new();

// Kernel32 hostname functions that Python might use
type GetComputerNameAFn = unsafe extern "system" fn(lpBuffer: *mut i8, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_A_ORIGINAL: OnceLock<&GetComputerNameAFn> = OnceLock::new();

type GetComputerNameWFn = unsafe extern "system" fn(lpBuffer: *mut u16, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_W_ORIGINAL: OnceLock<&GetComputerNameWFn> = OnceLock::new();

// Additional hostname functions that Python might use
type GetComputerNameExAFn = unsafe extern "system" fn(name_type: u32, lpBuffer: *mut i8, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_EX_A_ORIGINAL: OnceLock<&GetComputerNameExAFn> = OnceLock::new();

type GetComputerNameExWFn = unsafe extern "system" fn(name_type: u32, lpBuffer: *mut u16, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_EX_W_ORIGINAL: OnceLock<&GetComputerNameExWFn> = OnceLock::new();

// Add WSAStartup hook to debug socket initialization issues
type WSAStartupFn = unsafe extern "system" fn(wVersionRequested: u16, lpWSAData: *mut u8) -> i32;
static WSA_STARTUP_ORIGINAL: OnceLock<&WSAStartupFn> = OnceLock::new();

/// Windows socket hook for socket creation
unsafe extern "system" fn socket_detour(af: INT, r#type: INT, protocol: INT) -> SOCKET {
    tracing::trace!("socket_detour -> af: {}, type: {}, protocol: {}", af, r#type, protocol);
    
    // Call the original function to create the socket
    let original = SOCKET_ORIGINAL.get().unwrap();
    let socket = unsafe { original(af, r#type, protocol) };
    
    if socket != INVALID_SOCKET {
        // Determine socket kind
        let socket_kind = match r#type {
            SOCK_STREAM => WindowsSocketKind::Tcp,
            SOCK_DGRAM => WindowsSocketKind::Udp,
            _ => {
                // For unknown socket types, don't manage them with mirrord
                tracing::trace!("socket_detour -> unknown socket type {}, not managing", r#type);
                return socket;
            }
        };
        
        // Only handle IPv4 and IPv6 sockets
        if af == AF_INET || af == AF_INET6 {
            let user_socket = Arc::new(WindowsUserSocket {
                domain: af,
                kind: socket_kind,
                state: WindowsSocketState::Initialized,
            });
            
            if let Ok(mut sockets) = WINDOWS_SOCKETS.lock() {
                sockets.insert(socket, user_socket);
                tracing::trace!("socket_detour -> registered socket {} with mirrord", socket);
            } else {
                tracing::error!("socket_detour -> failed to lock WINDOWS_SOCKETS");
            }
        }
    }
    
    socket
}

/// Windows socket hook for bind
unsafe extern "system" fn bind_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("bind_detour -> socket: {}, namelen: {}", s, namelen);
    
    // Check if this socket is managed by mirrord (don't remove it yet)
    let is_managed = {
        if let Ok(sockets) = WINDOWS_SOCKETS.lock() {
            sockets.contains_key(&s)
        } else {
            false
        }
    };
    
    if is_managed {
        // Convert Windows sockaddr to Rust SocketAddr
        if let Some(requested_addr) = unsafe { sockaddr_to_socket_addr(name, namelen) } {
            tracing::info!("bind_detour -> mirrord binding socket to {}", requested_addr);
            
            // For now, we'll do a simple bind to the same address
            // In a full implementation, this would check mirrord config and potentially
            // bind to different addresses based on incoming traffic configuration
            
            let original = BIND_ORIGINAL.get().unwrap();
            let result = unsafe { original(s, name, namelen) };
            
            if result == 0 {
                // Success - update socket state atomically
                if let Ok(mut sockets) = WINDOWS_SOCKETS.lock() {
                    if let Some(socket_ref) = sockets.get_mut(&s) {
                        if let Some(socket_mut) = Arc::get_mut(socket_ref) {
                            let bound = WindowsSocketBound {
                                requested_address: requested_addr,
                                address: requested_addr, // For now, assume same address
                            };
                            socket_mut.state = WindowsSocketState::Bound(bound);
                            tracing::info!("bind_detour -> socket {} bound through mirrord", s);
                        } else {
                            // Socket is shared, need to create new instance
                            let mut new_socket = (**socket_ref).clone();
                            let bound = WindowsSocketBound {
                                requested_address: requested_addr,
                                address: requested_addr,
                            };
                            new_socket.state = WindowsSocketState::Bound(bound);
                            sockets.insert(s, Arc::new(new_socket));
                            tracing::info!("bind_detour -> socket {} bound through mirrord (cloned)", s);
                        }
                    }
                }
            }
            
            return result;
        }
    }
    
    // Fall back to original function for non-managed sockets or errors
    let original = BIND_ORIGINAL.get().unwrap();
    unsafe { original(s, name, namelen) }
}

/// Windows socket hook for listen
unsafe extern "system" fn listen_detour(s: SOCKET, backlog: INT) -> INT {
    tracing::trace!("listen_detour -> socket: {}, backlog: {}", s, backlog);
    
    // Check if this socket is managed by mirrord and in bound state
    let should_subscribe = {
        if let Ok(sockets) = WINDOWS_SOCKETS.lock() {
            if let Some(user_socket) = sockets.get(&s) {
                match &user_socket.state {
                    WindowsSocketState::Bound(bound) => {
                        tracing::info!("listen_detour -> mirrord socket {} transitioning to listening on {}", 
                                     s, bound.address);
                        Some(bound.address)
                    },
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        }
    };
    
    if let Some(bind_addr) = should_subscribe {
        // Subscribe to port for incoming traffic
        let port_subscribe = PortSubscribe {
            listening_on: bind_addr,
            subscription: PortSubscription::Steal(StealType::All(bind_addr.port())),
        };
        
        match make_windows_proxy_request_with_response(port_subscribe) {
            Ok(_) => {
                tracing::info!("listen_detour -> successfully subscribed to port {}", bind_addr.port());
            }
            Err(e) => {
                tracing::error!("listen_detour -> failed to subscribe to port {}: {}", bind_addr.port(), e);
                // Continue with original listen anyway
            }
        }
        
        // Call original listen
        let original = LISTEN_ORIGINAL.get().unwrap();
        let result = unsafe { original(s, backlog) };
        
        if result == 0 {
            // Success - update socket state to listening
            if let Ok(mut sockets) = WINDOWS_SOCKETS.lock() {
                if let Some(socket_ref) = sockets.get_mut(&s) {
                    if let Some(socket_mut) = Arc::get_mut(socket_ref) {
                        if let WindowsSocketState::Bound(bound) = &socket_mut.state {
                            socket_mut.state = WindowsSocketState::Listening(bound.clone());
                            tracing::info!("listen_detour -> socket {} now listening through mirrord", s);
                        }
                    }
                }
            }
        }
        
        return result;
    }
    
    // Fall back to original function for non-managed sockets
    let original = LISTEN_ORIGINAL.get().unwrap();
    unsafe { original(s, backlog) }
}

/// Windows socket hook for connect
unsafe extern "system" fn connect_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("connect_detour -> socket: {}, namelen: {}", s, namelen);
    
    // Check if this socket is managed by mirrord
    let managed_socket = {
        if let Ok(sockets) = WINDOWS_SOCKETS.lock() {
            sockets.get(&s).cloned()
        } else {
            None
        }
    };
    
    if let Some(user_socket) = managed_socket {
        // Convert Windows sockaddr to Rust SocketAddr
        if let Some(remote_addr) = unsafe { sockaddr_to_socket_addr(name, namelen) } {
            tracing::info!("connect_detour -> mirrord intercepting connection to {}", remote_addr);
            
            // Create the proxy request
            let protocol = match user_socket.kind {
                WindowsSocketKind::Tcp => NetProtocol::Stream,
                WindowsSocketKind::Udp => NetProtocol::Datagrams,
            };
            
            let remote_address = match SocketAddress::try_from(remote_addr) {
                Ok(addr) => addr,
                Err(e) => {
                    tracing::error!("connect_detour -> failed to convert address: {:?}", e);
                    // Fall back to original function
                    let original = CONNECT_ORIGINAL.get().unwrap();
                    return unsafe { original(s, name, namelen) };
                }
            };
            
            let request = OutgoingConnectRequest {
                remote_address: remote_address.clone(),
                protocol,
            };
            
            // Make the proxy request
            match make_windows_proxy_request_with_response(request) {
                Ok(_response) => {
                    // Since make_request_with_response is currently a placeholder that returns an error,
                    // this branch won't actually be reached. When the full implementation is done,
                    // we would handle the response here and connect to the layer address.
                    tracing::info!("connect_detour -> got proxy response (placeholder implementation)");
                    
                    // For now, fall back to original connect
                    let original = CONNECT_ORIGINAL.get().unwrap();
                    return unsafe { original(s, name, namelen) };
                }
                Err(e) => {
                    tracing::debug!("connect_detour -> proxy request failed (expected): {:?}", e);
                    
                    // Fall back to original connect
                    let original = CONNECT_ORIGINAL.get().unwrap();
                    return unsafe { original(s, name, namelen) };
                }
            }
        }
    }
    
    // Fallback to original function for all error cases and non-managed sockets
    let original = CONNECT_ORIGINAL.get().unwrap();
    unsafe { original(s, name, namelen) }
}

/// Windows socket hook for accept
unsafe extern "system" fn accept_detour(s: SOCKET, addr: *mut SOCKADDR, addrlen: *mut INT) -> SOCKET {
    tracing::trace!("accept_detour -> socket: {}", s);
    
    // Call original accept first
    let original = ACCEPT_ORIGINAL.get().unwrap();
    let accepted_socket = unsafe { original(s, addr, addrlen) };
    
    if accepted_socket != INVALID_SOCKET {
        // Check if the listening socket is managed by mirrord
        let listening_socket_info = {
            if let Ok(sockets) = WINDOWS_SOCKETS.lock() {
                sockets.get(&s).cloned()
            } else {
                None
            }
        };
        
        if let Some(listening_socket) = listening_socket_info {
            match &listening_socket.state {
                WindowsSocketState::Listening(bound) => {
                    tracing::info!("accept_detour -> accepted socket {} from mirrord-managed listener", accepted_socket);
                    
                    // Create a new managed socket for the accepted connection
                    // Initially in "connected" state since accept implies connection
                    let peer_addr = if !addr.is_null() && !addrlen.is_null() {
                        unsafe { sockaddr_to_socket_addr(addr as *const SOCKADDR, *addrlen) }
                    } else {
                        None
                    };
                    
                    if let Some(peer_address) = peer_addr {
                        let connected = WindowsSocketConnected {
                            remote_address: SocketAddress::try_from(peer_address).unwrap_or_else(|_| {
                                SocketAddress::try_from(std::net::SocketAddr::new(
                                    std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0
                                )).unwrap()
                            }),
                            local_address: SocketAddress::try_from(bound.address).unwrap_or_else(|_| {
                                SocketAddress::try_from(std::net::SocketAddr::new(
                                    std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0
                                )).unwrap()
                            }),
                            layer_address: None,
                        };
                        
                        let accepted_user_socket = Arc::new(WindowsUserSocket {
                            domain: listening_socket.domain,
                            kind: listening_socket.kind.clone(),
                            state: WindowsSocketState::Connected(connected),
                        });
                        
                        if let Ok(mut sockets) = WINDOWS_SOCKETS.lock() {
                            sockets.insert(accepted_socket, accepted_user_socket);
                            tracing::info!("accept_detour -> registered accepted socket {} with mirrord", accepted_socket);
                        }
                    }
                }
                _ => {
                    tracing::trace!("accept_detour -> listening socket not in listening state");
                }
            }
        }
    }
    
    accepted_socket
}

/// Windows socket hook for getsockname
unsafe extern "system" fn getsockname_detour(s: SOCKET, name: *mut SOCKADDR, namelen: *mut INT) -> INT {
    tracing::trace!("getsockname_detour -> socket: {}", s);
    
    // TODO: Implement mirrord getsockname logic here
    let original = GETSOCKNAME_ORIGINAL.get().unwrap();
    unsafe {
        original(s, name, namelen)
    }
}

/// Windows socket hook for getpeername
unsafe extern "system" fn getpeername_detour(s: SOCKET, name: *mut SOCKADDR, namelen: *mut INT) -> INT {
    tracing::trace!("getpeername_detour -> socket: {}", s);
    
    // TODO: Implement mirrord getpeername logic here
    let original = GETPEERNAME_ORIGINAL.get().unwrap();
    unsafe {
        original(s, name, namelen)
    }
}

/// Windows socket hook for gethostname
unsafe extern "system" fn gethostname_detour(name: *mut i8, namelen: INT) -> INT {
    tracing::debug!("gethostname_detour called with namelen: {}", namelen);
    
    // ALWAYS call the original first to see if that's working
    let original = GETHOSTNAME_ORIGINAL.get().unwrap();
    let result = unsafe { original(name, namelen) };
    
    if result != 0 {
        // Original failed, return the error
        return result;
    }
    
    // Original succeeded, now try to override the result if we have a remote hostname
    let resolver = DefaultHostnameResolver::remote();
    match get_or_init_hostname(&resolver) {
        HostnameResult::Success(hostname) => {
            // Return the actual remote hostname (pod name) for testing
            let hostname_str = hostname.to_string_lossy();
            let hostname_bytes = hostname_str.as_bytes();
            
            // Check if the buffer is large enough (include null terminator)
            if hostname_bytes.len() + 1 <= namelen as usize {
                unsafe {
                    ptr::copy_nonoverlapping(
                        hostname_bytes.as_ptr() as *const i8,
                        name,
                        hostname_bytes.len(),
                    );
                    // Add null terminator
                    *name.add(hostname_bytes.len()) = 0;
                }
                return 0; // Success
            } else {
                // Keep the original result since it fit
                return 0;
            }
        }
        HostnameResult::UseLocal | HostnameResult::Error(_) => {
            // Keep the original result
            return 0;
        }
    }
}

/// Simple pass-through hook for WSAStartup to debug socket initialization
unsafe extern "system" fn wsa_startup_detour(wVersionRequested: u16, lpWSAData: *mut u8) -> i32 {
    tracing::debug!("WSAStartup called with version: {}", wVersionRequested);
    
    let original = WSA_STARTUP_ORIGINAL.get().unwrap();
    let result = unsafe { original(wVersionRequested, lpWSAData) };
    
    if result != 0 {
        tracing::warn!("WSAStartup failed with error: {}", result);
    }
    
    result
}

/// Windows kernel32 hook for GetComputerNameA
unsafe extern "system" fn get_computer_name_a_detour(lpBuffer: *mut i8, nSize: *mut u32) -> i32 {
    let original = GET_COMPUTER_NAME_A_ORIGINAL.get().unwrap();
    unsafe { handle_hostname_ansi(lpBuffer, nSize, |buf, size| unsafe { original(buf, size) }, "GetComputerNameA") }
}

/// Windows kernel32 hook for GetComputerNameW
unsafe extern "system" fn get_computer_name_w_detour(lpBuffer: *mut u16, nSize: *mut u32) -> i32 {
    let original = GET_COMPUTER_NAME_W_ORIGINAL.get().unwrap();
    unsafe { handle_hostname_unicode(lpBuffer, nSize, |buf, size| unsafe { original(buf, size) }, "GetComputerNameW") }
}

/// Windows kernel32 hook for GetComputerNameExA
unsafe extern "system" fn get_computer_name_ex_a_detour(name_type: u32, lpBuffer: *mut i8, nSize: *mut u32) -> i32 {
    tracing::debug!("GetComputerNameExA hook called with name_type: {}", name_type);
    
    const COMPUTER_NAME_PHYSICAL_DNS_HOSTNAME: u32 = 5;
    
    // Only handle the DNS hostname type
    if name_type == COMPUTER_NAME_PHYSICAL_DNS_HOSTNAME {
        let original = GET_COMPUTER_NAME_EX_A_ORIGINAL.get().unwrap();
        return unsafe { handle_hostname_ansi(lpBuffer, nSize, |buf, size| unsafe { original(name_type, buf, size) }, "GetComputerNameExA") };
    }
    
    // For other name types, call original function
    let original = GET_COMPUTER_NAME_EX_A_ORIGINAL.get().unwrap();
    unsafe { original(name_type, lpBuffer, nSize) }
}

/// Windows kernel32 hook for GetComputerNameExW
unsafe extern "system" fn get_computer_name_ex_w_detour(name_type: u32, lpBuffer: *mut u16, nSize: *mut u32) -> i32 {
    tracing::debug!("GetComputerNameExW hook called with name_type: {}", name_type);
    
    const COMPUTER_NAME_PHYSICAL_DNS_HOSTNAME: u32 = 5;
    
    // Only handle the DNS hostname type
    if name_type == COMPUTER_NAME_PHYSICAL_DNS_HOSTNAME {
        let original = GET_COMPUTER_NAME_EX_W_ORIGINAL.get().unwrap();
        return unsafe { handle_hostname_unicode(lpBuffer, nSize, |buf, size| unsafe { original(name_type, buf, size) }, "GetComputerNameExW") };
    }
    
    // For other name types, call original function
    let original = GET_COMPUTER_NAME_EX_W_ORIGINAL.get().unwrap();
    unsafe { original(name_type, lpBuffer, nSize) }
}

/// Hook for gethostbyname to handle DNS resolution of our modified hostname
unsafe extern "system" fn gethostbyname_detour(name: *const i8) -> *mut HOSTENT {
    tracing::debug!("gethostbyname_detour called");
    
    if name.is_null() {
        tracing::debug!("gethostbyname: name is null, calling original");
        return unsafe { GETHOSTBYNAME_ORIGINAL.get().unwrap()(name) };
    }
    
    // SAFETY: Validate the string pointer before dereferencing
    let hostname_cstr = match unsafe { std::ffi::CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            tracing::debug!("gethostbyname: invalid UTF-8 in hostname, calling original");
            return unsafe { GETHOSTBYNAME_ORIGINAL.get().unwrap()(name) };
        }
    };
    
    tracing::debug!("gethostbyname: resolving hostname: {}", hostname_cstr);
    
    // Check if this is our remote hostname
    if is_remote_hostname(hostname_cstr) {
        tracing::debug!("gethostbyname: intercepting resolution for our hostname: {}", hostname_cstr);
        
        // Check if we have a cached mapping for this hostname
        match REMOTE_DNS_REVERSE_MAPPING.lock() {
            Ok(mapping) => {
                if let Some(target_ip) = mapping.get(hostname_cstr) {
                    tracing::debug!("gethostbyname: found cached IP mapping {} -> {}", hostname_cstr, target_ip);
                    
                    // Try to resolve the target IP address using original function
                    if let Ok(target_cstr) = std::ffi::CString::new(target_ip.as_str()) {
                        // SECURITY: Validate the cached IP is actually an IP address, not a hostname
                        if target_ip.parse::<std::net::IpAddr>().is_ok() || target_ip == "localhost" {
                            let result = unsafe { GETHOSTBYNAME_ORIGINAL.get().unwrap()(target_cstr.as_ptr()) };
                            if !result.is_null() {
                                tracing::debug!("gethostbyname: successfully resolved cached mapping");
                                return result;
                            }
                        } else {
                            tracing::error!("gethostbyname: cached entry '{}' is not a valid IP address, removing", target_ip);
                            // Note: We can't modify the mapping here since we only have a read lock
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("gethostbyname: failed to acquire cache lock: {}", e);
            }
        }
        
        // Try to resolve with fallback logic
        if let Some(fallback_hostname) = resolve_hostname_with_fallback(hostname_cstr) {
            let result = unsafe { GETHOSTBYNAME_ORIGINAL.get().unwrap()(fallback_hostname.as_ptr()) };
            if !result.is_null() {
                tracing::debug!("gethostbyname: successfully resolved with fallback");
                
                // Cache this mapping for future use
                match REMOTE_DNS_REVERSE_MAPPING.lock() {
                    Ok(mut mapping) => {
                        // Extract IP from HOSTENT for caching
                        if let Some(ip_str) = unsafe { extract_ip_from_hostent(result) } {
                            // Check cache size limit and evict if necessary
                            evict_old_dns_entries(&mut mapping);
                            mapping.insert(hostname_cstr.to_string(), ip_str);
                            tracing::debug!("gethostbyname: cached mapping {} -> IP", hostname_cstr);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("gethostbyname: failed to acquire cache lock for storing: {}", e);
                    }
                }
                return result;
            }
        }
    }
    
    // For all other hostnames or if our hostname resolution fails, call original function
    tracing::debug!("gethostbyname: calling original function for hostname: {}", hostname_cstr);
    unsafe { GETHOSTBYNAME_ORIGINAL.get().unwrap()(name) }
}

/// Hook for getaddrinfo to handle DNS resolution with full mirrord functionality
/// 
/// This follows the same pattern as the Unix layer but uses Windows types and calling conventions.
/// It converts Windows ADDRINFOA structures and makes DNS requests through the mirrord agent.
unsafe extern "system" fn getaddrinfo_detour(
    raw_node: *const i8,
    raw_service: *const i8,
    raw_hints: *const ADDRINFOA,
    out_addr_info: *mut *mut ADDRINFOA
) -> INT {
    tracing::debug!("getaddrinfo_detour called");
    
    unsafe {
        // Convert raw pointers to safe Rust types, mirroring Unix implementation
        let rawish_node = (!raw_node.is_null()).then(|| std::ffi::CStr::from_ptr(raw_node));
        let rawish_service = (!raw_service.is_null()).then(|| std::ffi::CStr::from_ptr(raw_service));
        let rawish_hints = raw_hints.as_ref();

        // Try to get the hostname for mirrord DNS resolution
        match windows_getaddrinfo(rawish_node, rawish_service, rawish_hints) {
            Ok(c_addr_info_ptr) => {
                // Success - store the result and track it for cleanup
                out_addr_info.copy_from_nonoverlapping(&c_addr_info_ptr, 1);
                MANAGED_ADDRINFO
                    .lock()
                    .expect("MANAGED_ADDRINFO lock failed")
                    .insert(c_addr_info_ptr as usize);
                0 // Success
            }
            Err(_) => {
                // Fall back to original Windows getaddrinfo
                tracing::debug!("getaddrinfo: falling back to original Windows function");
                GETADDRINFO_ORIGINAL.get().unwrap()(raw_node, raw_service, raw_hints, out_addr_info)
            }
        }
    }
}

/// Deallocates ADDRINFOA structures that were allocated by our getaddrinfo_detour.
/// 
/// This follows the same pattern as the Unix layer - it checks if the structure
/// was allocated by us and frees it properly, or calls the original freeaddrinfo if it wasn't ours.
unsafe extern "system" fn freeaddrinfo_detour(addrinfo: *mut ADDRINFOA) {
    unsafe {
        if !free_managed_addrinfo(addrinfo) {
            // Not one of ours - call original freeaddrinfo
            FREEADDRINFO_ORIGINAL.get().unwrap()(addrinfo);
        }
    }
}

/// Initialize socket hooks by setting up detours for Windows socket functions
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    tracing::info!("Initializing socket hooks");
    
    // Register core socket operations
    apply_hook!(
        guard,
        "ws2_32",
        "socket",
        socket_detour,
        SocketFn,
        SOCKET_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "bind",
        bind_detour,
        BindFn,
        BIND_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "listen",
        listen_detour,
        ListenFn,
        LISTEN_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "connect",
        connect_detour,
        ConnectFn,
        CONNECT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "accept",
        accept_detour,
        AcceptFn,
        ACCEPT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "getsockname",
        getsockname_detour,
        GetsocknameFn,
        GETSOCKNAME_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "getpeername",
        getpeername_detour,
        GetpeernameFn,
        GETPEERNAME_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "gethostname",
        gethostname_detour,
        GethostnameFn,
        GETHOSTNAME_ORIGINAL
    )?;

    // Register GetComputerNameExW hook - this is what Python's socket.gethostname() actually uses
    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameExW",
        get_computer_name_ex_w_detour,
        GetComputerNameExWFn,
        GET_COMPUTER_NAME_EX_W_ORIGINAL
    )?;
    
    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameExA",
        get_computer_name_ex_a_detour,
        GetComputerNameExAFn,
        GET_COMPUTER_NAME_EX_A_ORIGINAL
    )?;
    
    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameA",
        get_computer_name_a_detour,
        GetComputerNameAFn,
        GET_COMPUTER_NAME_A_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameW",
        get_computer_name_w_detour,
        GetComputerNameWFn,
        GET_COMPUTER_NAME_W_ORIGINAL
    )?;

    // Add WSAStartup hook to debug socket initialization issues
    apply_hook!(
        guard,
        "ws2_32",
        "WSAStartup",
        wsa_startup_detour,
        WSAStartupFn,
        WSA_STARTUP_ORIGINAL
    )?;

    // Add DNS resolution hooks to handle our modified hostnames
    apply_hook!(
        guard,
        "ws2_32",
        "gethostbyname",
        gethostbyname_detour,
        GethostbynameFn,
        GETHOSTBYNAME_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "getaddrinfo",
        getaddrinfo_detour,
        GetaddrinfoFn,
        GETADDRINFO_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "freeaddrinfo",
        freeaddrinfo_detour,
        FreeaddrinfoFn,
        FREEADDRINFO_ORIGINAL
    )?;

    tracing::info!("Socket hooks initialized successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sockaddr_conversion_safety() {
        // Test null pointer
        let result = unsafe { sockaddr_to_socket_addr(std::ptr::null(), 0) };
        assert!(result.is_none());
        
        // Test invalid length
        let dummy_addr = std::mem::MaybeUninit::<SOCKADDR>::uninit();
        let result = unsafe { sockaddr_to_socket_addr(dummy_addr.as_ptr(), -1) };
        assert!(result.is_none());
    }
}

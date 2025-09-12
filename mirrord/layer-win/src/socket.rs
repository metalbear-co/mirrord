//! Module responsible for registering hooks targeting socket operation syscalls.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]

mod hostname;
mod state;
mod utils;

use std::{
    ffi::CStr,
    net::{IpAddr, SocketAddr},
    sync::OnceLock,
};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::socket::{Bound, DnsResolver, SocketKind, SocketState};
use winapi::{
    shared::{
        minwindef::INT,
        ntdef::PCSTR,
        ws2def::{ADDRINFOA, ADDRINFOW, AF_INET, AF_INET6, SOCKADDR},
    },
    um::{
        errhandlingapi::SetLastError,
        sysinfoapi::{
            ComputerNameDnsDomain, ComputerNameDnsFullyQualified, ComputerNameDnsHostname,
            ComputerNameMax, ComputerNameNetBIOS, ComputerNamePhysicalDnsDomain,
            ComputerNamePhysicalDnsFullyQualified, ComputerNamePhysicalDnsHostname,
            ComputerNamePhysicalNetBIOS,
        },
        winsock2::{HOSTENT, INVALID_SOCKET, SOCKET, SOCKET_ERROR, fd_set, timeval},
    },
};
use windows_strings::PCWSTR;

use self::{
    hostname::{
        MANAGED_ADDRINFO, REMOTE_DNS_REVERSE_MAPPING, free_managed_addrinfo, handle_hostname_ansi,
        handle_hostname_unicode, is_remote_hostname, resolve_hostname_with_fallback,
        windows_getaddrinfo,
    },
    state::{
        SOCKET_MANAGER, log_connection_result, proxy_bind, register_accepted_socket,
        setup_listening,
    },
    utils::{
        ManagedAddrInfoAny, SocketAddrExtWin, SocketAddressExtWin, evict_old_cache_entries,
        extract_ip_from_hostent,
    },
};
use crate::{apply_hook, layer_config};

/// Windows-specific DNS resolver implementation
struct WindowsDnsResolver;

impl DnsResolver for WindowsDnsResolver {
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn resolve_hostname(
        &self,
        hostname: &str,
        _port: u16,
        _family: i32,
        _protocol: i32,
    ) -> Result<Vec<IpAddr>, Self::Error> {
        // Use the existing Windows remote DNS resolution
        match hostname::remote_dns_resolve(hostname) {
            Ok(records) => Ok(records.into_iter().map(|(_, ip)| ip).collect()),
            Err(e) => {
                tracing::debug!("Remote DNS resolution failed for {}: {}", hostname, e);
                // Fallback to local resolution as in Unix layer
                use std::net::ToSocketAddrs;
                match (hostname, 0u16).to_socket_addrs() {
                    Ok(addresses) => Ok(addresses.map(|addr| addr.ip()).collect()),
                    Err(local_err) => {
                        tracing::debug!(
                            "Local DNS resolution also failed for {}: {}",
                            hostname,
                            local_err
                        );
                        Ok(vec![]) // No records found
                    }
                }
            }
        }
    }

    fn remote_dns_enabled(&self) -> bool {
        crate::layer_config().feature.network.dns.enabled
    }
}

// Function type definitions for original Windows socket functions
type SocketType = unsafe extern "system" fn(af: INT, r#type: INT, protocol: INT) -> SOCKET;
static SOCKET_ORIGINAL: OnceLock<&SocketType> = OnceLock::new();

type BindType = unsafe extern "system" fn(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT;
static BIND_ORIGINAL: OnceLock<&BindType> = OnceLock::new();

type ListenType = unsafe extern "system" fn(s: SOCKET, backlog: INT) -> INT;
static LISTEN_ORIGINAL: OnceLock<&ListenType> = OnceLock::new();

type ConnectType = unsafe extern "system" fn(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT;
static CONNECT_ORIGINAL: OnceLock<&ConnectType> = OnceLock::new();

type AcceptType =
    unsafe extern "system" fn(s: SOCKET, addr: *mut SOCKADDR, addrlen: *mut INT) -> SOCKET;
static ACCEPT_ORIGINAL: OnceLock<&AcceptType> = OnceLock::new();

type GetSockNameType =
    unsafe extern "system" fn(s: SOCKET, name: *mut SOCKADDR, namelen: *mut INT) -> INT;
static GET_SOCK_NAME_ORIGINAL: OnceLock<&GetSockNameType> = OnceLock::new();

type GetPeerNameType =
    unsafe extern "system" fn(s: SOCKET, name: *mut SOCKADDR, namelen: *mut INT) -> INT;
static GET_PEER_NAME_ORIGINAL: OnceLock<&GetPeerNameType> = OnceLock::new();

type GetHostNameType = unsafe extern "system" fn(name: *mut i8, namelen: INT) -> INT;
static GET_HOST_NAME_ORIGINAL: OnceLock<&GetHostNameType> = OnceLock::new();

// DNS resolution functions that Python socket.gethostbyname uses
type GetHostByNameType = unsafe extern "system" fn(name: *const i8) -> *mut HOSTENT;
static GET_HOST_BY_NAME_ORIGINAL: OnceLock<&GetHostByNameType> = OnceLock::new();

type GetAddrInfoType = unsafe extern "system" fn(
    node_name: PCSTR,
    service_name: PCSTR,
    hints: *const ADDRINFOA,
    result: *mut *mut ADDRINFOA,
) -> INT;
static GET_ADDR_INFO_ORIGINAL: OnceLock<&GetAddrInfoType> = OnceLock::new();

type FreeAddrInfoType = unsafe extern "system" fn(addrinfo: *mut ADDRINFOA);
static FREE_ADDR_INFO_ORIGINAL: OnceLock<&FreeAddrInfoType> = OnceLock::new();

// Unicode versions that Python might use
type GetAddrInfoWType = unsafe extern "system" fn(
    node_name: PCWSTR,
    service_name: PCWSTR,
    hints: *const ADDRINFOW,
    result: *mut *mut ADDRINFOW,
) -> INT;
static GET_ADDR_INFO_W_ORIGINAL: OnceLock<&GetAddrInfoWType> = OnceLock::new();

// Kernel32 hostname functions that Python might use
type GetComputerNameAType = unsafe extern "system" fn(lpBuffer: *mut i8, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_A_ORIGINAL: OnceLock<&GetComputerNameAType> = OnceLock::new();

type GetComputerNameWType = unsafe extern "system" fn(lpBuffer: *mut u16, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_W_ORIGINAL: OnceLock<&GetComputerNameWType> = OnceLock::new();

// Additional hostname functions that Python might use
type GetComputerNameExAType =
    unsafe extern "system" fn(name_type: u32, lpBuffer: *mut i8, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_EX_A_ORIGINAL: OnceLock<&GetComputerNameExAType> = OnceLock::new();

type GetComputerNameExWType =
    unsafe extern "system" fn(name_type: u32, lpBuffer: *mut u16, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_EX_W_ORIGINAL: OnceLock<&GetComputerNameExWType> = OnceLock::new();

type WSAStartupType = unsafe extern "system" fn(wVersionRequested: u16, lpWSAData: *mut u8) -> i32;
static WSA_STARTUP_ORIGINAL: OnceLock<&WSAStartupType> = OnceLock::new();

// Add WSACleanup hook to complete Winsock lifecycle management
type WSACleanupType = unsafe extern "system" fn() -> i32;
static WSA_CLEANUP_ORIGINAL: OnceLock<&WSACleanupType> = OnceLock::new();

// ioctlsocket for socket I/O control (used for non-blocking mode)
type IoCtlSocketType = unsafe extern "system" fn(s: SOCKET, cmd: i32, argp: *mut u32) -> i32;
static IOCTL_SOCKET_ORIGINAL: OnceLock<&IoCtlSocketType> = OnceLock::new();

// select for socket readiness monitoring
type SelectType = unsafe extern "system" fn(
    nfds: i32,
    readfds: *mut fd_set,
    writefds: *mut fd_set,
    exceptfds: *mut fd_set,
    timeout: *const timeval,
) -> i32;
static SELECT_ORIGINAL: OnceLock<&SelectType> = OnceLock::new();

// WSAGetLastError for getting detailed error information
type WSAGetLastErrorType = unsafe extern "system" fn() -> i32;
static WSA_GET_LAST_ERROR_ORIGINAL: OnceLock<&WSAGetLastErrorType> = OnceLock::new();

// WSASocket for advanced socket creation (used by Node.js internally)
type WSASocketType = unsafe extern "system" fn(
    af: i32,
    socket_type: i32,
    protocol: i32,
    lpProtocolInfo: *mut u8,
    g: u32,
    dwFlags: u32,
) -> SOCKET;
static WSA_SOCKET_ORIGINAL: OnceLock<&WSASocketType> = OnceLock::new();

// WSA async I/O functions that Node.js uses for overlapped operations
type WSAConnectType = unsafe extern "system" fn(
    s: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    lpCallerData: *mut u8,
    lpCalleeData: *mut u8,
    lpSQOS: *mut u8,
    lpGQOS: *mut u8,
) -> INT;
static WSA_CONNECT_ORIGINAL: OnceLock<&WSAConnectType> = OnceLock::new();

type WSAAcceptType = unsafe extern "system" fn(
    s: SOCKET,
    addr: *mut SOCKADDR,
    addrlen: *mut INT,
    lpfnCondition: *mut u8,
    dwCallbackData: usize,
) -> SOCKET;
static WSA_ACCEPT_ORIGINAL: OnceLock<&WSAAcceptType> = OnceLock::new();

type WSASendType = unsafe extern "system" fn(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesSent: *mut u32,
    dwFlags: u32,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT;
static WSA_SEND_ORIGINAL: OnceLock<&WSASendType> = OnceLock::new();

type WSARecvType = unsafe extern "system" fn(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesRecvd: *mut u32,
    lpFlags: *mut u32,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT;
static WSA_RECV_ORIGINAL: OnceLock<&WSARecvType> = OnceLock::new();

type WSASendToType = unsafe extern "system" fn(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesSent: *mut u32,
    dwFlags: u32,
    lpTo: *const SOCKADDR,
    iTolen: INT,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT;
static WSA_SEND_TO_ORIGINAL: OnceLock<&WSASendToType> = OnceLock::new();

type WSARecvFromType = unsafe extern "system" fn(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesRecvd: *mut u32,
    lpFlags: *mut u32,
    lpFrom: *mut SOCKADDR,
    lpFromlen: *mut INT,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT;
static WSA_RECV_FROM_ORIGINAL: OnceLock<&WSARecvFromType> = OnceLock::new();

// Data transfer function types
type RecvType = unsafe extern "system" fn(s: SOCKET, buf: *mut i8, len: INT, flags: INT) -> INT;
static RECV_ORIGINAL: OnceLock<&RecvType> = OnceLock::new();

type SendType = unsafe extern "system" fn(s: SOCKET, buf: *const i8, len: INT, flags: INT) -> INT;
static SEND_ORIGINAL: OnceLock<&SendType> = OnceLock::new();

type RecvFromType = unsafe extern "system" fn(
    s: SOCKET,
    buf: *mut i8,
    len: INT,
    flags: INT,
    from: *mut SOCKADDR,
    fromlen: *mut INT,
) -> INT;
static RECV_FROM_ORIGINAL: OnceLock<&RecvFromType> = OnceLock::new();

type SendToType = unsafe extern "system" fn(
    s: SOCKET,
    buf: *const i8,
    len: INT,
    flags: INT,
    to: *const SOCKADDR,
    tolen: INT,
) -> INT;
static SEND_TO_ORIGINAL: OnceLock<&SendToType> = OnceLock::new();

// Socket management function types
type CloseSocketType = unsafe extern "system" fn(s: SOCKET) -> INT;
static CLOSE_SOCKET_ORIGINAL: OnceLock<&CloseSocketType> = OnceLock::new();

type ShutdownType = unsafe extern "system" fn(s: SOCKET, how: INT) -> INT;
static SHUTDOWN_ORIGINAL: OnceLock<&ShutdownType> = OnceLock::new();

// Socket option function types
type SetSockOptType = unsafe extern "system" fn(
    s: SOCKET,
    level: INT,
    optname: INT,
    optval: *const i8,
    optlen: INT,
) -> INT;
static SET_SOCK_OPT_ORIGINAL: OnceLock<&SetSockOptType> = OnceLock::new();

type GetSockOptType = unsafe extern "system" fn(
    s: SOCKET,
    level: INT,
    optname: INT,
    optval: *mut i8,
    optlen: *mut INT,
) -> INT;
static GET_SOCK_OPT_ORIGINAL: OnceLock<&GetSockOptType> = OnceLock::new();

/// Windows socket hook for socket creation
unsafe extern "system" fn socket_detour(af: INT, r#type: INT, protocol: INT) -> SOCKET {
    tracing::trace!(
        "socket_detour -> af: {}, type: {}, protocol: {}",
        af,
        r#type,
        protocol
    );

    // Call the original function to create the socket
    let original = SOCKET_ORIGINAL.get().unwrap();
    let socket = unsafe { original(af, r#type, protocol) };

    if socket != INVALID_SOCKET {
        if af == AF_INET || af == AF_INET6 {
            SOCKET_MANAGER.register_socket(socket, af, r#type, protocol);
            tracing::info!(
                "socket_detour -> registered socket {} with mirrord (af: {}, type: {})",
                socket,
                af,
                r#type
            );
        } else {
            tracing::debug!(
                "socket_detour -> skipping socket {} registration (unsupported af: {})",
                socket,
                af
            );
        }
    }

    socket
}

/// Windows socket hook for bind
unsafe extern "system" fn bind_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("bind_detour -> socket: {}, namelen: {}", s, namelen);

    // Check if this socket is managed by mirrord using SocketManager
    if SOCKET_MANAGER.is_socket_managed(s) {
        // Convert Windows sockaddr to Rust SocketAddr
        if let Some(requested_addr) = SocketAddr::try_from_raw(name, namelen) {
            tracing::info!(
                "bind_detour -> mirrord binding socket to {}",
                requested_addr
            );

            // Use proxy bind operation from state.rs
            match proxy_bind(s, requested_addr) {
                Ok(bound_addr) => {
                    let original = BIND_ORIGINAL.get().unwrap();
                    let result = unsafe { original(s, name, namelen) };

                    if result == 0 {
                        // Success - update socket state using SocketManager
                        let bound = Bound {
                            requested_address: bound_addr,
                            address: bound_addr,
                        };
                        SOCKET_MANAGER.set_socket_state(s, SocketState::Bound(bound));
                        tracing::info!("bind_detour -> socket {} bound through mirrord proxy", s);
                    }

                    return result;
                }
                Err(error_code) => {
                    tracing::error!("bind_detour -> proxy bind failed with error {}", error_code);
                    return SOCKET_ERROR;
                }
            }
        }
    }

    // Fall back to original function for non-managed sockets or errors
    let original = BIND_ORIGINAL.get().unwrap();

    unsafe { original(s, name, namelen) }
}

/// Windows socket hook for listen
unsafe extern "system" fn listen_detour(s: SOCKET, backlog: INT) -> INT {
    tracing::trace!("listen_detour -> socket: {}, backlog: {}", s, backlog);

    // Check if this socket is managed by mirrord and get bound address
    if let Some(bind_addr) = SOCKET_MANAGER.get_bound_address(s) {
        tracing::info!(
            "listen_detour -> mirrord socket {} transitioning to listening on {}",
            s,
            bind_addr
        );

        // Use setup_listening helper from state.rs
        match setup_listening(s, bind_addr, backlog) {
            Ok(()) => {
                // Call original listen
                let original = LISTEN_ORIGINAL.get().unwrap();
                let result = unsafe { original(s, backlog) };

                if result == 0 {
                    // Success - update socket state to listening using SocketManager
                    if SOCKET_MANAGER
                        .is_socket_in_state(s, |state| matches!(state, SocketState::Bound(_)))
                    {
                        // Get the bound state and transition to listening
                        if let Some(SocketState::Bound(bound)) = SOCKET_MANAGER.get_socket_state(s)
                        {
                            SOCKET_MANAGER.set_socket_state(s, SocketState::Listening(bound));
                            tracing::info!(
                                "listen_detour -> socket {} now listening through mirrord",
                                s
                            );
                        }
                    }
                }

                return result;
            }
            Err(e) => {
                tracing::error!("listen_detour -> setup_listening failed: {}", e);
                // Continue with original listen anyway
            }
        }

        // Fallback - call original listen even if setup failed
        let original = LISTEN_ORIGINAL.get().unwrap();

        unsafe { original(s, backlog) }
    } else {
        // Fall back to original function for non-managed sockets
        let original = LISTEN_ORIGINAL.get().unwrap();

        unsafe { original(s, backlog) }
    }
}

/// Windows socket hook for connect
unsafe extern "system" fn connect_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("connect_detour -> socket: {}, namelen: {}", s, namelen);

    match state::attempt_proxy_connection(s, name, namelen, "connect_detour") {
        state::ProxyConnectionResult::Success((sockaddr, sockaddr_len)) => {
            // Call the original function with the prepared sockaddr
            let original = CONNECT_ORIGINAL.get().unwrap();
            let result = unsafe { original(s, &sockaddr as *const SOCKADDR, sockaddr_len) };
            return log_connection_result(result, "connect_detour");
        }
        state::ProxyConnectionResult::Fallback => {
            // Fallback to original function
        }
    }

    // fallback to original
    let original = CONNECT_ORIGINAL.get().unwrap();
    let result = unsafe { original(s, name, namelen) };
    log_connection_result(result, "connect_detour")
}

/// Windows socket hook for accept
unsafe extern "system" fn accept_detour(
    s: SOCKET,
    addr: *mut SOCKADDR,
    addrlen: *mut INT,
) -> SOCKET {
    tracing::trace!("accept_detour -> socket: {}", s);

    // Call original accept first
    let original = ACCEPT_ORIGINAL.get().unwrap();
    let accepted_socket = unsafe { original(s, addr, addrlen) };

    if accepted_socket != INVALID_SOCKET {
        // Check if the listening socket is managed and get its bound address
        if let Some(bound_addr) = SOCKET_MANAGER.get_bound_address(s) {
            // Check if listening socket is in listening state
            if SOCKET_MANAGER
                .is_socket_in_state(s, |state| matches!(state, SocketState::Listening(_)))
            {
                tracing::info!(
                    "accept_detour -> accepted socket {} from mirrord-managed listener",
                    accepted_socket
                );

                // Get peer address from accept result
                let peer_addr = if !addr.is_null() && !addrlen.is_null() {
                    SocketAddr::try_from_raw(addr as *const SOCKADDR, unsafe { *addrlen })
                } else {
                    None
                };

                if let Some(peer_address) = peer_addr {
                    // Get domain and kind from listening socket
                    if let Some(listening_socket) = SOCKET_MANAGER.get_socket(s) {
                        // Use register_accepted_socket helper from state.rs
                        let socket_type = match listening_socket.kind {
                            SocketKind::Tcp(t) => t,
                            SocketKind::Udp(t) => t,
                        };
                        match register_accepted_socket(
                            accepted_socket,
                            listening_socket.domain,
                            socket_type,
                            peer_address,
                            bound_addr,
                        ) {
                            Ok(()) => {
                                tracing::info!(
                                    "accept_detour -> registered accepted socket {} with mirrord",
                                    accepted_socket
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "accept_detour -> failed to register accepted socket: {}",
                                    e
                                );
                            }
                        }
                    }
                }
            } else {
                tracing::trace!("accept_detour -> listening socket not in listening state");
            }
        }
    }

    accepted_socket
}

/// Windows socket hook for getsockname
unsafe extern "system" fn getsockname_detour(
    s: SOCKET,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> INT {
    tracing::trace!("getsockname_detour -> socket: {}", s);

    // Check if this socket is managed by mirrord and get its bound address
    if let Some(bound_addr) = SOCKET_MANAGER.get_bound_address(s) {
        // Return the bound address for bound/listening sockets
        if !name.is_null() && !namelen.is_null() {
            match unsafe { bound_addr.to_windows_sockaddr_checked(name, namelen) } {
                Ok(()) => {
                    tracing::trace!(
                        "getsockname_detour -> returned mirrord bound address: {}",
                        bound_addr
                    );
                    return 0; // Success
                }
                Err(error_code) => {
                    tracing::debug!(
                        "getsockname_detour -> failed to convert bound address: error {}",
                        error_code
                    );
                    return SOCKET_ERROR;
                }
            }
        }
    } else if let Some((_, local_addr, layer_addr)) = SOCKET_MANAGER.get_connected_addresses(s) {
        // Return the layer address for connected sockets if available, otherwise local address
        let addr_to_return = layer_addr.as_ref().unwrap_or(&local_addr);
        match unsafe { addr_to_return.to_sockaddr_checked(name, namelen) } {
            Ok(()) => {
                tracing::trace!("getsockname_detour -> returned mirrord local address");
                return 0; // Success
            }
            Err(e) => {
                tracing::debug!(
                    "getsockname_detour -> failed to convert layer address: {}",
                    e
                );
                return SOCKET_ERROR;
            }
        }
    } else if SOCKET_MANAGER.is_socket_managed(s) {
        // For other managed socket states, fall back to original
        tracing::trace!(
            "getsockname_detour -> managed socket not in bound/connected state, using original"
        );
    }

    // Fall back to original function for non-managed sockets or errors
    let original = GET_SOCK_NAME_ORIGINAL.get().unwrap();

    unsafe { original(s, name, namelen) }
}

/// Windows socket hook for getpeername
unsafe extern "system" fn getpeername_detour(
    s: SOCKET,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> INT {
    tracing::trace!("getpeername_detour -> socket: {}", s);

    // Check if this socket is managed and get connected addresses
    if let Some((remote_addr, _, _)) = SOCKET_MANAGER.get_connected_addresses(s) {
        // Return the remote address for connected sockets
        match unsafe { remote_addr.to_sockaddr_checked(name, namelen) } {
            Ok(()) => {
                tracing::trace!("getpeername_detour -> returned mirrord remote address");
                return 0; // Success
            }
            Err(e) => {
                tracing::debug!(
                    "getpeername_detour -> failed to convert remote address: {}",
                    e
                );
                return SOCKET_ERROR;
            }
        }
    } else if SOCKET_MANAGER.is_socket_managed(s) {
        tracing::trace!(
            "getpeername_detour -> managed socket not in connected state, using original"
        );
    }

    // Fall back to original function for non-managed sockets or errors
    let original = GET_PEER_NAME_ORIGINAL.get().unwrap();
    unsafe { original(s, name, namelen) }
}

/// Pass-through hook for WSAStartup
unsafe extern "system" fn wsa_startup_detour(wVersionRequested: u16, lpWSAData: *mut u8) -> i32 {
    tracing::debug!("WSAStartup called with version: {}", wVersionRequested);

    let original = WSA_STARTUP_ORIGINAL.get().unwrap();
    let result = unsafe { original(wVersionRequested, lpWSAData) };

    if result != 0 {
        tracing::warn!("WSAStartup failed with error: {}", result);
    }

    result
}

/// Pass-through hook for WSACleanup
unsafe extern "system" fn wsa_cleanup_detour() -> i32 {
    // Pass through to original - let Windows Sockets handle cleanup
    let original = WSA_CLEANUP_ORIGINAL.get().unwrap();
    unsafe { original() }
}

/// Socket management detour for ioctlsocket() - controls I/O mode of socket
unsafe extern "system" fn ioctlsocket_detour(s: SOCKET, cmd: i32, argp: *mut u32) -> i32 {
    // Pass through to original - interceptor handles I/O control for managed sockets
    let original = IOCTL_SOCKET_ORIGINAL.get().unwrap();
    unsafe { original(s, cmd, argp) }
}

/// Socket management detour for select() - monitors socket readiness
unsafe extern "system" fn select_detour(
    nfds: i32,
    readfds: *mut fd_set,
    writefds: *mut fd_set,
    exceptfds: *mut fd_set,
    timeout: *const timeval,
) -> i32 {
    // Pass through to original - interceptor handles I/O readiness for managed sockets
    let original = SELECT_ORIGINAL.get().unwrap();
    unsafe { original(nfds, readfds, writefds, exceptfds, timeout) }
}

/// Windows socket hook for WSAGetLastError (error information)
unsafe extern "system" fn wsa_get_last_error_detour() -> i32 {
    let original = WSA_GET_LAST_ERROR_ORIGINAL.get().unwrap();
    let result = unsafe { original() };

    tracing::trace!("wsa_get_last_error_detour -> error: {}", result);

    result
}

/// Windows socket hook for WSASocket (advanced socket creation)
unsafe extern "system" fn wsa_socket_detour(
    af: i32,
    socket_type: i32,
    protocol: i32,
    lpProtocolInfo: *mut u8,
    g: u32,
    dwFlags: u32,
) -> SOCKET {
    tracing::trace!(
        "wsa_socket_detour -> af: {}, type: {}, protocol: {}, flags: {}",
        af,
        socket_type,
        protocol,
        dwFlags
    );
    let original = WSA_SOCKET_ORIGINAL.get().unwrap();
    let socket = unsafe { original(af, socket_type, protocol, lpProtocolInfo, g, dwFlags) };
    if socket != INVALID_SOCKET {
        if af == AF_INET || af == AF_INET6 {
            SOCKET_MANAGER.register_socket(socket, af, socket_type, protocol);
        }
    } else {
        tracing::warn!("wsa_socket_detour -> failed to create socket");
    }
    socket
}

/// Windows socket hook for WSAConnect (asynchronous connect)
/// Node.js uses this for non-blocking connect operations
unsafe extern "system" fn wsa_connect_detour(
    s: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    lpCallerData: *mut u8,
    lpCalleeData: *mut u8,
    lpSQOS: *mut u8,
    lpGQOS: *mut u8,
) -> INT {
    tracing::trace!("wsa_connect_detour -> socket: {}, namelen: {}", s, namelen);

    // Attempt complete proxy connection flow
    match state::attempt_proxy_connection(s, name, namelen, "wsa_connect_detour") {
        state::ProxyConnectionResult::Success((sockaddr, sockaddr_len)) => {
            // Call the original function with the prepared sockaddr
            let original = WSA_CONNECT_ORIGINAL.get().unwrap();
            let result = unsafe {
                original(
                    s,
                    &sockaddr as *const SOCKADDR,
                    sockaddr_len,
                    lpCallerData,
                    lpCalleeData,
                    lpSQOS,
                    lpGQOS,
                )
            };
            return log_connection_result(result, "wsa_connect_detour");
        }
        state::ProxyConnectionResult::Fallback => {
            // Fallback to original function
        }
    }

    // Fallback to original function
    let original = WSA_CONNECT_ORIGINAL.get().unwrap();
    let result = unsafe { original(s, name, namelen, lpCallerData, lpCalleeData, lpSQOS, lpGQOS) };
    log_connection_result(result, "wsa_connect_detour")
}

/// Windows socket hook for WSAAccept (asynchronous accept)
/// Node.js uses this for non-blocking accept operations
unsafe extern "system" fn wsa_accept_detour(
    s: SOCKET,
    addr: *mut SOCKADDR,
    addrlen: *mut INT,
    lpfnCondition: *mut u8,
    dwCallbackData: usize,
) -> SOCKET {
    tracing::trace!("wsa_accept_detour -> socket: {}", s);

    // Pass through to original - interceptor handles data for managed sockets
    let original = WSA_ACCEPT_ORIGINAL.get().unwrap();
    unsafe { original(s, addr, addrlen, lpfnCondition, dwCallbackData) }
}

/// Windows socket hook for WSASend (asynchronous send)
/// Node.js uses this extensively for overlapped I/O operations
unsafe extern "system" fn wsa_send_detour(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesSent: *mut u32,
    dwFlags: u32,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT {
    tracing::trace!(
        "wsa_send_detour -> socket: {}, buffer_count: {}",
        s,
        dwBufferCount
    );

    // Check if this socket is managed by mirrord
    let managed_socket = SOCKET_MANAGER.get_socket(s);

    if let Some(user_socket) = managed_socket {
        tracing::debug!(
            "wsa_send_detour -> socket {} is managed, kind: {:?}",
            s,
            user_socket.kind
        );

        // Check if outgoing traffic is enabled for this socket type
        let should_intercept = match user_socket.kind {
            SocketKind::Tcp(_) => {
                let tcp_outgoing = crate::layer_config().feature.network.outgoing.tcp;
                tracing::info!("wsa_send_detour -> TCP outgoing enabled: {}", tcp_outgoing);
                tcp_outgoing
            }
            SocketKind::Udp(_) => {
                let udp_outgoing = crate::layer_config().feature.network.outgoing.udp;
                tracing::info!("wsa_send_detour -> UDP outgoing enabled: {}", udp_outgoing);
                udp_outgoing
            }
        };

        if !should_intercept {
            tracing::info!(
                "wsa_send_detour -> outgoing traffic disabled for {:?}, passing through to original (disabled mode)",
                user_socket.kind
            );
            // Pass through to original but mark that it should be blocked
            // The application will handle the response/lack thereof
        } else {
            tracing::debug!("wsa_send_detour -> intercepting enabled, allowing send operation");
        }
    }

    // Pass through to original - interceptor handles data routing for managed sockets
    let original = WSA_SEND_ORIGINAL.get().unwrap();
    unsafe {
        original(
            s,
            lpBuffers,
            dwBufferCount,
            lpNumberOfBytesSent,
            dwFlags,
            lpOverlapped,
            lpCompletionRoutine,
        )
    }
}

/// Windows socket hook for WSARecv (asynchronous receive)
/// Node.js uses this extensively for overlapped I/O operations
unsafe extern "system" fn wsa_recv_detour(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesRecvd: *mut u32,
    lpFlags: *mut u32,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT {
    tracing::trace!(
        "wsa_recv_detour -> socket: {}, buffer_count: {}",
        s,
        dwBufferCount
    );

    // Pass through to original - interceptor handles data routing for managed sockets
    let original = WSA_RECV_ORIGINAL.get().unwrap();
    unsafe {
        original(
            s,
            lpBuffers,
            dwBufferCount,
            lpNumberOfBytesRecvd,
            lpFlags,
            lpOverlapped,
            lpCompletionRoutine,
        )
    }
}

/// Windows socket hook for WSASendTo (asynchronous UDP send)
/// Node.js uses this for overlapped UDP operations
unsafe extern "system" fn wsa_send_to_detour(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesSent: *mut u32,
    dwFlags: u32,
    lpTo: *const SOCKADDR,
    iTolen: INT,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT {
    tracing::trace!(
        "wsa_send_to_detour -> socket: {}, buffer_count: {}, to_len: {}",
        s,
        dwBufferCount,
        iTolen
    );

    // Check if this socket is managed by mirrord
    let managed_socket = SOCKET_MANAGER.get_socket(s);

    if let Some(user_socket) = managed_socket {
        tracing::debug!(
            "wsa_send_to_detour -> socket {} is managed, kind: {:?}",
            s,
            user_socket.kind
        );

        // Check if outgoing traffic is enabled for this socket type
        let should_intercept = match user_socket.kind {
            SocketKind::Tcp(_) => {
                let tcp_outgoing = crate::layer_config().feature.network.outgoing.tcp;
                tracing::info!(
                    "wsa_send_to_detour -> TCP outgoing enabled: {}",
                    tcp_outgoing
                );
                tcp_outgoing
            }
            SocketKind::Udp(_) => {
                let udp_outgoing = crate::layer_config().feature.network.outgoing.udp;
                tracing::info!(
                    "wsa_send_to_detour -> UDP outgoing enabled: {}",
                    udp_outgoing
                );
                udp_outgoing
            }
        };

        if !should_intercept {
            tracing::info!(
                "wsa_send_to_detour -> outgoing traffic disabled for {:?}, passing through to original (disabled mode)",
                user_socket.kind
            );
            // Pass through to original but mark that it should be blocked
            // The application will handle the response/lack thereof
        } else {
            tracing::debug!("wsa_send_to_detour -> intercepting enabled, allowing send operation");
        }
    }

    // Pass through to original - interceptor handles data routing for managed sockets
    let original = WSA_SEND_TO_ORIGINAL.get().unwrap();
    unsafe {
        original(
            s,
            lpBuffers,
            dwBufferCount,
            lpNumberOfBytesSent,
            dwFlags,
            lpTo,
            iTolen,
            lpOverlapped,
            lpCompletionRoutine,
        )
    }
}

/// Windows socket hook for WSARecvFrom (asynchronous UDP receive)
/// Node.js uses this for overlapped UDP operations
unsafe extern "system" fn wsa_recv_from_detour(
    s: SOCKET,
    lpBuffers: *mut u8,
    dwBufferCount: u32,
    lpNumberOfBytesRecvd: *mut u32,
    lpFlags: *mut u32,
    lpFrom: *mut SOCKADDR,
    lpFromlen: *mut INT,
    lpOverlapped: *mut u8,
    lpCompletionRoutine: *mut u8,
) -> INT {
    tracing::trace!(
        "wsa_recv_from_detour -> socket: {}, buffer_count: {}",
        s,
        dwBufferCount
    );

    // Pass through to original - interceptor handles data routing for managed sockets
    let original = WSA_RECV_FROM_ORIGINAL.get().unwrap();
    unsafe {
        original(
            s,
            lpBuffers,
            dwBufferCount,
            lpNumberOfBytesRecvd,
            lpFlags,
            lpFrom,
            lpFromlen,
            lpOverlapped,
            lpCompletionRoutine,
        )
    }
}

/// Windows winsock hook for gethostname
unsafe extern "system" fn gethostname_detour(name: *mut i8, namelen: INT) -> INT {
    tracing::debug!("gethostname_detour called with namelen: {}", namelen);

    // Validate parameters
    if name.is_null() || namelen <= 0 {
        tracing::debug!("gethostname: invalid parameters");
        return SOCKET_ERROR;
    }

    // Check if hostname feature is enabled
    let hostname_enabled = crate::layer_config().feature.hostname;
    tracing::debug!(
        "gethostname: hostname feature enabled: {}",
        hostname_enabled
    );

    // Try to get the remote hostname first only if hostname feature is enabled
    if hostname_enabled {
        match hostname::get_hostname_with_fallback() {
            Some(remote_hostname) => {
                tracing::debug!("gethostname: got remote hostname: '{}'", remote_hostname);

                let hostname_bytes = remote_hostname.as_bytes();
                let required_len = hostname_bytes.len();

                // Check if buffer is large enough
                if (namelen as usize) < required_len {
                    tracing::debug!(
                        "gethostname: buffer too small, need {} bytes, have {}",
                        required_len,
                        namelen
                    );
                    return SOCKET_ERROR;
                }

                // Copy hostname to buffer
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        hostname_bytes.as_ptr(),
                        name as *mut u8,
                        hostname_bytes.len(),
                    );
                    *(name.add(hostname_bytes.len())) = 0;
                }

                tracing::debug!(
                    "gethostname: returning remote hostname: '{}'",
                    remote_hostname
                );
                return 0; // Success
            }
            None => {
                tracing::error!("gethostname: no remote hostname available");
                return SOCKET_ERROR;
            }
        };
    }

    // Fall back to original function if hostname feature is disabled or no remote hostname
    // available
    tracing::debug!(
        "gethostname: hostname feature disabled or no remote hostname available, calling original"
    );
    let original = GET_HOST_NAME_ORIGINAL.get().unwrap();
    unsafe { original(name, namelen) }
}

/// Windows kernel32 hook for GetComputerNameA
unsafe extern "system" fn get_computer_name_a_detour(lpBuffer: *mut i8, nSize: *mut u32) -> i32 {
    let original = GET_COMPUTER_NAME_A_ORIGINAL.get().unwrap();
    unsafe {
        handle_hostname_ansi(
            lpBuffer,
            nSize,
            |buf, size| original(buf, size),
            "GetComputerNameA",
        )
    }
}

/// Windows kernel32 hook for GetComputerNameW
unsafe extern "system" fn get_computer_name_w_detour(lpBuffer: *mut u16, nSize: *mut u32) -> i32 {
    let original = GET_COMPUTER_NAME_W_ORIGINAL.get().unwrap();
    unsafe {
        handle_hostname_unicode(
            lpBuffer,
            nSize,
            |buf, size| original(buf, size),
            "GetComputerNameW",
        )
    }
}

/// Windows kernel32 hook for GetComputerNameExA
unsafe extern "system" fn get_computer_name_ex_a_detour(
    name_type: u32,
    lpBuffer: *mut i8,
    nSize: *mut u32,
) -> i32 {
    use winapi::um::{errhandlingapi::SetLastError, sysinfoapi::*};

    tracing::debug!(
        "GetComputerNameExA hook called with name_type: {}",
        name_type
    );

    const ERROR_MORE_DATA: u32 = 234;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    // Validate input parameters first
    if nSize.is_null() {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
        }
        return 0; // FALSE
    }

    // Handle invalid name_type (ComputerNameMax and beyond)
    if name_type >= ComputerNameMax {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
        }
        return 0; // FALSE
    }

    // For hostname-related types that Python uses, try to return our remote hostname
    let should_intercept = match name_type {
        ComputerNameDnsHostname
        | ComputerNameDnsFullyQualified
        | ComputerNamePhysicalDnsHostname
        | ComputerNamePhysicalDnsFullyQualified => true,
        _ => false,
    };

    if should_intercept {
        // Check if hostname feature is enabled
        let hostname_enabled = layer_config().feature.hostname;
        tracing::debug!(
            "GetComputerNameExA: hostname feature enabled: {}",
            hostname_enabled
        );

        if hostname_enabled {
            match hostname::get_hostname_with_fallback() {
                Some(dns_name) => {
                    tracing::debug!("GetComputerNameExA: got remote hostname: '{}'", dns_name);

                    // Determine what form of the hostname to return based on name_type
                    let hostname_to_return = match name_type {
                        ComputerNameNetBIOS | ComputerNamePhysicalNetBIOS => {
                            // Return uppercase version for NetBIOS types
                            dns_name.to_uppercase()
                        }
                        _ => {
                            // Return as-is for DNS types
                            dns_name
                        }
                    };

                    let dns_bytes = hostname_to_return.as_bytes();
                    let required_size = dns_bytes.len(); // Size without null terminator
                    let current_buffer_size = unsafe { *nSize } as usize;

                    tracing::debug!(
                        "GetComputerNameExA: hostname_to_return='{}', len={}, buffer_size={}",
                        hostname_to_return,
                        required_size,
                        current_buffer_size
                    );

                    // Check if buffer is large enough (needs space for characters + null
                    // terminator)
                    if current_buffer_size < required_size + 1 {
                        // Buffer too small - set required size and return ERROR_MORE_DATA
                        unsafe {
                            *nSize = (required_size + 1) as u32; // Include null terminator in required size
                        }
                        unsafe {
                            SetLastError(ERROR_MORE_DATA);
                        }
                        tracing::debug!(
                            "GetComputerNameExA: buffer too small for name_type {}, need {} chars, have {}",
                            name_type,
                            required_size + 1,
                            current_buffer_size
                        );
                        return 0; // FALSE
                    }

                    // Buffer is large enough - copy the hostname
                    if !lpBuffer.is_null() && current_buffer_size > 0 {
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                dns_bytes.as_ptr(),
                                lpBuffer as *mut u8,
                                dns_bytes.len(),
                            );
                            // Add null terminator
                            *(lpBuffer.add(dns_bytes.len())) = 0;
                            *nSize = required_size as u32; // Set actual length (excluding null terminator)
                        }
                        tracing::debug!(
                            "GetComputerNameExA: returning remote hostname for name_type {}: '{}' ({} chars)",
                            name_type,
                            hostname_to_return,
                            required_size
                        );
                        return 1; // TRUE - Success
                    } else {
                        // Invalid buffer pointer
                        unsafe {
                            SetLastError(ERROR_INVALID_PARAMETER);
                        }
                        return 0; // FALSE
                    }
                }
                None => {
                    tracing::error!("GetComputerNameExA: no remote hostname available");
                    // fall back to original
                }
            }
        }

        // If hostname feature is disabled or we can't get remote hostname, fall back to original
        tracing::debug!(
            "GetComputerNameExA: hostname feature disabled or no remote hostname available for name_type {}, falling back to original",
            name_type
        );
        // fall back to original
    }

    // For domain types, NetBIOS types when we don't have a hostname, or other name types, call
    // original function
    let original = GET_COMPUTER_NAME_EX_A_ORIGINAL.get().unwrap();
    unsafe { original(name_type, lpBuffer, nSize) }
}

/// Windows kernel32 hook for GetComputerNameExW
unsafe extern "system" fn get_computer_name_ex_w_detour(
    name_type: u32,
    lpBuffer: *mut u16,
    nSize: *mut u32,
) -> i32 {
    tracing::debug!(
        "GetComputerNameExW hook called with name_type: {}",
        name_type
    );

    const ERROR_MORE_DATA: u32 = 234;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    // Validate input parameters first
    if nSize.is_null() {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
        }
        return 0; // FALSE
    }

    // Handle valid name types (0-7)
    if name_type < ComputerNameMax {
        // Check if hostname feature is enabled
        let hostname_enabled = layer_config().feature.hostname;
        tracing::debug!(
            "GetComputerNameExW: hostname feature enabled: {}",
            hostname_enabled
        );

        if hostname_enabled {
            // Try to get the remote hostname for supported types
            match hostname::get_hostname_with_fallback() {
                Some(hostname) => {
                    // Transform hostname based on name_type
                    let result_name = match name_type {
                        ComputerNameNetBIOS | ComputerNamePhysicalNetBIOS => {
                            hostname.to_uppercase()
                        } /* NetBIOS variants - uppercase */
                        ComputerNameDnsHostname
                        | ComputerNameDnsFullyQualified
                        | ComputerNamePhysicalDnsHostname
                        | ComputerNamePhysicalDnsFullyQualified => hostname, /* DNS variants - */
                        // as-is
                        ComputerNameDnsDomain | ComputerNamePhysicalDnsDomain => String::new(), /* Domain variants - empty (no domain info available) */
                        _ => unreachable!(),
                    };

                    // Convert to UTF-16
                    let name_utf16: Vec<u16> = result_name.encode_utf16().collect();
                    let required_size = name_utf16.len();

                    let current_buffer_size = unsafe { *nSize } as usize;

                    // Check if buffer is large enough (needs space for characters + null
                    // terminator)
                    if current_buffer_size < name_utf16.len() {
                        // Buffer too small - set required size and return ERROR_MORE_DATA
                        unsafe {
                            *nSize = required_size as u32;
                        }
                        unsafe {
                            SetLastError(ERROR_MORE_DATA);
                        }
                        tracing::debug!(
                            "GetComputerNameExW: buffer too small, need {} chars, have {}",
                            required_size,
                            current_buffer_size
                        );
                        return 0; // FALSE
                    }

                    // Buffer is large enough - copy the hostname
                    if !lpBuffer.is_null() && current_buffer_size > 0 {
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                name_utf16.as_ptr(),
                                lpBuffer,
                                name_utf16.len(),
                            );
                            *nSize = required_size as u32; // Set actual length (excluding null terminator)
                        }
                        tracing::debug!(
                            "GetComputerNameExW: returning remote hostname for type {}: '{}' ({} chars)",
                            name_type,
                            result_name,
                            required_size
                        );
                        tracing::error!("GetComputerNameExW: WSAGetLastError: {}", unsafe {
                            WSA_GET_LAST_ERROR_ORIGINAL.get().unwrap()()
                        });
                        return 1; // TRUE - Success
                    } else {
                        // Invalid buffer pointer
                        unsafe {
                            SetLastError(ERROR_INVALID_PARAMETER);
                        }
                        return 0; // FALSE
                    }
                }
                None => {
                    tracing::error!("GetComputerNameExW: no remote hostname available");
                    // fall back to original
                }
            }
        }

        // If hostname feature is disabled or we can't get remote hostname, fall back to original
        tracing::debug!(
            "GetComputerNameExW: hostname feature disabled or no remote hostname available, falling back to original"
        );
        let original = GET_COMPUTER_NAME_EX_W_ORIGINAL.get().unwrap();
        return unsafe { original(name_type, lpBuffer, nSize) };
    }

    // Invalid name_type (ComputerNameMax and above)
    unsafe {
        SetLastError(ERROR_INVALID_PARAMETER);
    }
    0 // FALSE
}

/// Hook for gethostbyname to handle DNS resolution of our modified hostname
unsafe extern "system" fn gethostbyname_detour(name: *const i8) -> *mut HOSTENT {
    if name.is_null() {
        tracing::debug!("gethostbyname: name is null, calling original");
        return unsafe { GET_HOST_BY_NAME_ORIGINAL.get().unwrap()(name) };
    }

    // SAFETY: Validate the string pointer before dereferencing
    let hostname_cstr = match unsafe { std::ffi::CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            tracing::debug!("gethostbyname: invalid UTF-8 in hostname, calling original");
            return unsafe { GET_HOST_BY_NAME_ORIGINAL.get().unwrap()(name) };
        }
    };

    tracing::debug!("gethostbyname: resolving hostname: {}", hostname_cstr);

    // Check if this is our remote hostname
    if is_remote_hostname(hostname_cstr) {
        tracing::debug!(
            "gethostbyname: intercepting resolution for our hostname: {}",
            hostname_cstr
        );

        // Check if we have a cached mapping for this hostname
        match REMOTE_DNS_REVERSE_MAPPING.lock() {
            Ok(mapping) => {
                if let Some(target_ip) = mapping.get(hostname_cstr) {
                    tracing::debug!(
                        "gethostbyname: found cached IP mapping {} -> {}",
                        hostname_cstr,
                        target_ip
                    );

                    // Try to resolve the target IP address using original function
                    if let Ok(target_cstr) = std::ffi::CString::new(target_ip.as_str()) {
                        // SECURITY: Validate the cached IP is actually an IP address, not a
                        // hostname
                        if target_ip.parse::<std::net::IpAddr>().is_ok() || target_ip == "localhost"
                        {
                            let result = unsafe {
                                GET_HOST_BY_NAME_ORIGINAL.get().unwrap()(target_cstr.as_ptr())
                            };
                            if !result.is_null() {
                                tracing::debug!(
                                    "gethostbyname: successfully resolved cached mapping"
                                );
                                return result;
                            }
                        } else {
                            tracing::error!(
                                "gethostbyname: cached entry '{}' is not a valid IP address, removing",
                                target_ip
                            );
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
            let result =
                unsafe { GET_HOST_BY_NAME_ORIGINAL.get().unwrap()(fallback_hostname.as_ptr()) };
            if !result.is_null() {
                tracing::debug!("gethostbyname: successfully resolved with fallback");

                // Cache this mapping for future use
                match REMOTE_DNS_REVERSE_MAPPING.lock() {
                    Ok(mut mapping) => {
                        // Extract IP from HOSTENT for caching
                        if let Some(ip_str) = unsafe { extract_ip_from_hostent(result) } {
                            // Check cache size limit and evict if necessary
                            evict_old_cache_entries(&mut mapping, 1000);
                            mapping.insert(hostname_cstr.to_string(), ip_str);
                            tracing::debug!(
                                "gethostbyname: cached mapping {} -> IP",
                                hostname_cstr
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "gethostbyname: failed to acquire cache lock for storing: {}",
                            e
                        );
                    }
                }
                return result;
            }
        }
    }

    // For all other hostnames or if our hostname resolution fails, call original function
    tracing::debug!(
        "gethostbyname: calling original function for hostname: {}",
        hostname_cstr
    );
    unsafe { GET_HOST_BY_NAME_ORIGINAL.get().unwrap()(name) }
}

/// Hook for getaddrinfo to handle DNS resolution with full mirrord functionality
///
/// This follows the same pattern as the Unix layer but uses Windows types and calling conventions.
/// It converts Windows ADDRINFOA structures and makes DNS requests through the mirrord agent.
unsafe extern "system" fn getaddrinfo_detour(
    raw_node: PCSTR,
    raw_service: PCSTR,
    raw_hints: *const ADDRINFOA,
    out_addr_info: *mut *mut ADDRINFOA,
) -> INT {
    let node_opt = match Option::from(raw_node) {
        Some(ptr) => {
            let cstr = unsafe { CStr::from_ptr(ptr) };
            Some(str_win::u8_buffer_to_string(cstr.to_bytes()))
        }
        None => None,
    };
    tracing::warn!("getaddrinfo_detour called for hostname: {:?}", node_opt);

    let service_opt = match Option::from(raw_service) {
        Some(ptr) => {
            let cstr = unsafe { CStr::from_ptr(ptr) };
            Some(str_win::u8_buffer_to_string(cstr.to_bytes()))
        }
        None => None,
    };

    let hints_ref = unsafe { raw_hints.as_ref() };

    unsafe {
        match windows_getaddrinfo::<ADDRINFOA>(node_opt, service_opt, hints_ref) {
            Ok(managed_addr_info) => {
                // Store the managed result pointer and move the object to MANAGED_ADDRINFO
                let addr_ptr = managed_addr_info.as_ptr();
                MANAGED_ADDRINFO
                    .lock()
                    .expect("MANAGED_ADDRINFO lock failed")
                    .insert(addr_ptr as usize, ManagedAddrInfoAny::A(managed_addr_info));
                *out_addr_info = addr_ptr;
                0 // Success
            }
            Err(_) => {
                // Fall back to original Windows getaddrinfo
                tracing::debug!("getaddrinfo: falling back to original Windows function");
                GET_ADDR_INFO_ORIGINAL.get().unwrap()(
                    raw_node,
                    raw_service,
                    raw_hints,
                    out_addr_info,
                )
            }
        }
    }
}

/// Hook for GetAddrInfoW (Unicode version) to handle DNS resolution
unsafe extern "system" fn getaddrinfow_detour(
    node_name: PCWSTR,
    service_name: PCWSTR,
    hints: *const ADDRINFOW,
    result: *mut *mut ADDRINFOW,
) -> INT {
    tracing::warn!("GetAddrInfoW_detour called");

    let node_opt = Option::from(node_name).map(|ptr| unsafe { str_win::u16_buffer_to_string(ptr.as_wide()) });
    tracing::warn!("GetAddrInfoW_detour called for hostname: {:?}", node_opt);

    let service_opt = Option::from(service_name).map(|ptr| unsafe { str_win::u16_buffer_to_string(ptr.as_wide()) });

    let hints_ref = unsafe { hints.as_ref() };

    // Use full windows_getaddrinfo approach with DNS selector logic and service/hints support
    match windows_getaddrinfo::<ADDRINFOW>(node_opt.clone(), service_opt, hints_ref) {
        Ok(managed_result) => {
            tracing::debug!("GetAddrInfoW: full resolution succeeded");
            // Store the managed result pointer and move the object to MANAGED_ADDRINFO
            let result_ptr = managed_result.as_ptr();
            unsafe { *result = result_ptr };
            MANAGED_ADDRINFO
                .lock()
                .expect("MANAGED_ADDRINFO lock failed")
                .insert(result_ptr as usize, ManagedAddrInfoAny::W(managed_result));
            return 0; // Success
        }
        Err(e) => {
            tracing::warn!(
                "GetAddrInfoW: resolution failed for '{:?}': {}",
                node_opt,
                e
            );
            // Fall through to original function
        }
    }

    // For all other hostnames or if conversion fails, call original function
    tracing::debug!(
        "GetAddrInfoW: calling original function for hostname: {:?}",
        node_opt
    );
    unsafe { GET_ADDR_INFO_W_ORIGINAL.get().unwrap()(node_name, service_name, hints, result) }
}

/// Deallocates ADDRINFOA structures that were allocated by our getaddrinfo_detour.
///
/// This follows the same pattern as the Unix layer - it checks if the structure
/// was allocated by us and frees it properly, or calls the original freeaddrinfo if it wasn't ours.
unsafe extern "system" fn freeaddrinfo_detour(addrinfo: *mut ADDRINFOA) {
    unsafe {
        if !free_managed_addrinfo(addrinfo) {
            // Not one of ours - call original freeaddrinfo
            FREE_ADDR_INFO_ORIGINAL.get().unwrap()(addrinfo);
        }
    }
}

// TODO: freeaddrinfow

/// Data transfer detour for recv() - receives data from a socket
///
/// Note: For mirrord-managed outgoing connections, data flows automatically through
/// the interceptor. This detour just passes through to the original recv() which
/// operates on the socket connected to the interceptor.
unsafe extern "system" fn recv_detour(s: SOCKET, buf: *mut i8, len: INT, flags: INT) -> INT {
    // Pass through to original - interceptor handles data routing for managed sockets
    let original = RECV_ORIGINAL.get().unwrap();
    unsafe { original(s, buf, len, flags) }
}

/// Data transfer detour for send() - sends data to a socket
///
/// Note: For mirrord-managed outgoing connections, data flows automatically through
/// the interceptor. This detour just passes through to the original send() which
/// operates on the socket connected to the interceptor.
unsafe extern "system" fn send_detour(s: SOCKET, buf: *const i8, len: INT, flags: INT) -> INT {
    // Pass through to original - interceptor handles data routing for managed sockets
    let original = SEND_ORIGINAL.get().unwrap();
    unsafe { original(s, buf, len, flags) }
}

/// Data transfer detour for recvfrom() - receives data from a socket with source address
///
/// Note: UDP/datagram sockets typically aren't managed by mirrord outgoing connections,
/// so this is primarily a pass-through for compatibility.
unsafe extern "system" fn recvfrom_detour(
    s: SOCKET,
    buf: *mut i8,
    len: INT,
    flags: INT,
    from: *mut SOCKADDR,
    fromlen: *mut INT,
) -> INT {
    // Pass through to original
    let original = RECV_FROM_ORIGINAL.get().unwrap();
    unsafe { original(s, buf, len, flags, from, fromlen) }
}

/// Data transfer detour for sendto() - sends data to a socket with destination address
///
/// Note: UDP/datagram sockets typically aren't managed by mirrord outgoing connections,
/// so this is primarily a pass-through for compatibility.
unsafe extern "system" fn sendto_detour(
    s: SOCKET,
    buf: *const i8,
    len: INT,
    flags: INT,
    to: *const SOCKADDR,
    tolen: INT,
) -> INT {
    // Pass through to original
    let original = SEND_TO_ORIGINAL.get().unwrap();
    unsafe { original(s, buf, len, flags, to, tolen) }
}

/// Socket management detour for closesocket() - closes a socket
unsafe extern "system" fn closesocket_detour(s: SOCKET) -> INT {
    // Clean up mirrord state for managed sockets
    SOCKET_MANAGER.remove_socket(s);

    // Call the original function
    let original = CLOSE_SOCKET_ORIGINAL.get().unwrap();
    unsafe { original(s) }
}

/// Socket management detour for shutdown() - shuts down part or all of a socket connection
unsafe extern "system" fn shutdown_detour(s: SOCKET, how: INT) -> INT {
    // Pass through to original - interceptor handles connection shutdown for managed sockets
    let original = SHUTDOWN_ORIGINAL.get().unwrap();
    unsafe { original(s, how) }
}

/// Socket option detour for setsockopt() - sets socket options
unsafe extern "system" fn setsockopt_detour(
    s: SOCKET,
    level: INT,
    optname: INT,
    optval: *const i8,
    optlen: INT,
) -> INT {
    // Pass through to original - interceptor handles socket option management for managed sockets
    let original = SET_SOCK_OPT_ORIGINAL.get().unwrap();
    unsafe { original(s, level, optname, optval, optlen) }
}

/// Socket option detour for getsockopt() - gets socket options
unsafe extern "system" fn getsockopt_detour(
    s: SOCKET,
    level: INT,
    optname: INT,
    optval: *mut i8,
    optlen: *mut INT,
) -> INT {
    // Pass through to original - interceptor handles socket option queries for managed sockets
    let original = GET_SOCK_OPT_ORIGINAL.get().unwrap();
    unsafe { original(s, level, optname, optval, optlen) }
}

/// Initialize socket hooks by setting up detours for Windows socket functions
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    println!("HOOK INIT DEBUG: Initializing socket hooks");

    // Register core socket operations
    println!("HOOK INIT DEBUG: Installing socket hook");
    apply_hook!(
        guard,
        "ws2_32",
        "socket",
        socket_detour,
        SocketType,
        SOCKET_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "bind",
        bind_detour,
        BindType,
        BIND_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "listen",
        listen_detour,
        ListenType,
        LISTEN_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "connect",
        connect_detour,
        ConnectType,
        CONNECT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "accept",
        accept_detour,
        AcceptType,
        ACCEPT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "getsockname",
        getsockname_detour,
        GetSockNameType,
        GET_SOCK_NAME_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "getpeername",
        getpeername_detour,
        GetPeerNameType,
        GET_PEER_NAME_ORIGINAL
    )?;

    // Register GetComputerNameExW hook - this is what Python's socket.gethostname() actually uses
    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameExW",
        get_computer_name_ex_w_detour,
        GetComputerNameExWType,
        GET_COMPUTER_NAME_EX_W_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameExA",
        get_computer_name_ex_a_detour,
        GetComputerNameExAType,
        GET_COMPUTER_NAME_EX_A_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameA",
        get_computer_name_a_detour,
        GetComputerNameAType,
        GET_COMPUTER_NAME_A_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "kernel32",
        "GetComputerNameW",
        get_computer_name_w_detour,
        GetComputerNameWType,
        GET_COMPUTER_NAME_W_ORIGINAL
    )?;

    // WSAStartup hook
    apply_hook!(
        guard,
        "ws2_32",
        "WSAStartup",
        wsa_startup_detour,
        WSAStartupType,
        WSA_STARTUP_ORIGINAL
    )?;

    // Add WSACleanup hook to complete Winsock lifecycle management
    apply_hook!(
        guard,
        "ws2_32",
        "WSACleanup",
        wsa_cleanup_detour,
        WSACleanupType,
        WSA_CLEANUP_ORIGINAL
    )?;

    // Add DNS resolution hooks to handle our modified hostnames
    tracing::warn!("Installing gethostbyname hook");
    apply_hook!(
        guard,
        "ws2_32",
        "gethostbyname",
        gethostbyname_detour,
        GetHostByNameType,
        GET_HOST_BY_NAME_ORIGINAL
    )?;

    tracing::warn!("Installing gethostname hook");
    apply_hook!(
        guard,
        "ws2_32",
        "gethostname",
        gethostname_detour,
        GetHostNameType,
        GET_HOST_NAME_ORIGINAL
    )?;

    tracing::warn!("Installing getaddrinfo hook");
    apply_hook!(
        guard,
        "ws2_32",
        "getaddrinfo",
        getaddrinfo_detour,
        GetAddrInfoType,
        GET_ADDR_INFO_ORIGINAL
    )?;

    tracing::warn!("Installing GetAddrInfoW hook");
    apply_hook!(
        guard,
        "ws2_32",
        "GetAddrInfoW",
        getaddrinfow_detour,
        GetAddrInfoWType,
        GET_ADDR_INFO_W_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "freeaddrinfo",
        freeaddrinfo_detour,
        FreeAddrInfoType,
        FREE_ADDR_INFO_ORIGINAL
    )?;

    // Register data transfer hooks
    apply_hook!(
        guard,
        "ws2_32",
        "recv",
        recv_detour,
        RecvType,
        RECV_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "send",
        send_detour,
        SendType,
        SEND_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "recvfrom",
        recvfrom_detour,
        RecvFromType,
        RECV_FROM_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "sendto",
        sendto_detour,
        SendToType,
        SEND_TO_ORIGINAL
    )?;

    // Register socket management hooks
    apply_hook!(
        guard,
        "ws2_32",
        "closesocket",
        closesocket_detour,
        CloseSocketType,
        CLOSE_SOCKET_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "shutdown",
        shutdown_detour,
        ShutdownType,
        SHUTDOWN_ORIGINAL
    )?;

    // Register socket option hooks
    apply_hook!(
        guard,
        "ws2_32",
        "setsockopt",
        setsockopt_detour,
        SetSockOptType,
        SET_SOCK_OPT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "getsockopt",
        getsockopt_detour,
        GetSockOptType,
        GET_SOCK_OPT_ORIGINAL
    )?;

    // Register additional I/O control and monitoring hooks
    apply_hook!(
        guard,
        "ws2_32",
        "ioctlsocket",
        ioctlsocket_detour,
        IoCtlSocketType,
        IOCTL_SOCKET_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "select",
        select_detour,
        SelectType,
        SELECT_ORIGINAL
    )?;

    // Register advanced Windows socket APIs
    apply_hook!(
        guard,
        "ws2_32",
        "WSAGetLastError",
        wsa_get_last_error_detour,
        WSAGetLastErrorType,
        WSA_GET_LAST_ERROR_ORIGINAL
    )?;

    tracing::warn!("Installing WSASocketA hook");
    apply_hook!(
        guard,
        "ws2_32",
        "WSASocketA",
        wsa_socket_detour,
        WSASocketType,
        WSA_SOCKET_ORIGINAL
    )?;

    // Register Node.js specific WSA async I/O hooks
    apply_hook!(
        guard,
        "ws2_32",
        "WSAConnect",
        wsa_connect_detour,
        WSAConnectType,
        WSA_CONNECT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "WSAAccept",
        wsa_accept_detour,
        WSAAcceptType,
        WSA_ACCEPT_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "WSASend",
        wsa_send_detour,
        WSASendType,
        WSA_SEND_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "WSARecv",
        wsa_recv_detour,
        WSARecvType,
        WSA_RECV_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "WSASendTo",
        wsa_send_to_detour,
        WSASendToType,
        WSA_SEND_TO_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "WSARecvFrom",
        wsa_recv_from_detour,
        WSARecvFromType,
        WSA_RECV_FROM_ORIGINAL
    )?;

    tracing::info!(
        "Socket hooks initialized successfully (including Node.js WSA async I/O support)"
    );
    Ok(())
}

//! Module responsible for registering hooks targeting socket operation syscalls.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::too_many_arguments)]

mod hostname;
mod ops;
mod state;
mod utils;

use std::{net::SocketAddr, sync::OnceLock};

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    error::{ConnectError, HookError, HookResult, SendToError, windows::WindowsError},
    proxy_connection::make_proxy_request_with_response,
    socket::{
        Bound, ConnectResult, SocketDescriptor, SocketKind, SocketState, get_bound_address,
        get_connected_addresses, get_socket, get_socket_state,
        hostname::{get_remote_hostname, remote_dns_resolve_via_proxy},
        is_socket_in_state, is_socket_managed, remove_socket, send_to, set_socket_state,
    },
};
use socket2::SockAddr;
use winapi::{
    ctypes::c_void,
    um::{
        minwinbase::OVERLAPPED,
        winsock2::{LPWSAOVERLAPPED_COMPLETION_ROUTINE, WSAOVERLAPPED},
    },
};
use winapi::{
    shared::{
        minwindef::{BOOL, FALSE, INT, TRUE},
        winerror::{ERROR_BUFFER_OVERFLOW, ERROR_MORE_DATA},
        ws2def::{ADDRINFOA, ADDRINFOW, AF_INET, AF_INET6, SOCKADDR},
    },
    um::{
        synchapi::SetEvent,
        winsock2::{
            HOSTENT, INVALID_SOCKET, SOCKET, SOCKET_ERROR, WSA_IO_PENDING, WSAEFAULT,
            WSAGetLastError, WSASetLastError, fd_set, timeval,
        },
    },
    // ws2tcpip::{GetNameInfoW, socklen_t},
};
use windows::Win32::{
    Foundation::ERROR_SUCCESS, Networking::WinSock::SIO_GET_EXTENSION_FUNCTION_POINTER,
};
use windows_strings::{PCSTR, PCWSTR};

const ERROR_SUCCESS_I32: i32 = ERROR_SUCCESS.0 as i32;

use self::{
    hostname::{
        MANAGED_ADDRINFO, free_managed_addrinfo, handle_hostname_ansi, handle_hostname_unicode,
        is_remote_hostname, windows_getaddrinfo,
    },
    ops::{
        WSABufferData, get_connectex_original, handle_connectex_extension_pointer,
        log_connection_result,
    },
    state::{proxy_bind, register_accepted_socket, register_windows_socket, setup_listening},
    utils::{ManagedAddrInfoAny, SocketAddrExtWin, create_thread_local_hostent},
};
use crate::{apply_hook, layer_setup};

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
    node_name: *const u8,
    service_name: *const u8,
    hints: *const ADDRINFOA,
    result: *mut *mut ADDRINFOA,
) -> INT;
static GET_ADDR_INFO_ORIGINAL: OnceLock<&GetAddrInfoType> = OnceLock::new();

type GetAddrInfoWType = unsafe extern "system" fn(
    node_name: *const u16,
    service_name: *const u16,
    hints: *const ADDRINFOW,
    result: *mut *mut ADDRINFOW,
) -> INT;
static GET_ADDR_INFO_W_ORIGINAL: OnceLock<&GetAddrInfoWType> = OnceLock::new();

// See comment about FreeAddrInfoW in apply_hook! below
// type FreeAddrInfoType = unsafe extern "system" fn(addrinfo: *mut ADDRINFOA);
// static FREE_ADDR_INFO_ORIGINAL: OnceLock<&FreeAddrInfoType> = OnceLock::new();
type FreeAddrInfoWType = unsafe extern "system" fn(addrinfo: *mut ADDRINFOW);
static FREE_ADDR_INFO_W_ORIGINAL: OnceLock<&FreeAddrInfoWType> = OnceLock::new();

// Kernel32 hostname functions that Python might use
type GetComputerNameAType = unsafe extern "system" fn(lpBuffer: *mut i8, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_A_ORIGINAL: OnceLock<&GetComputerNameAType> = OnceLock::new();

type GetComputerNameWType = unsafe extern "system" fn(lpBuffer: *mut u16, nSize: *mut u32) -> BOOL;
static GET_COMPUTER_NAME_W_ORIGINAL: OnceLock<&GetComputerNameWType> = OnceLock::new();

// Additional hostname functions that Python might use
type GetComputerNameExAType =
    unsafe extern "system" fn(name_type: u32, lpBuffer: *mut i8, nSize: *mut u32) -> i32;
static GET_COMPUTER_NAME_EX_A_ORIGINAL: OnceLock<&GetComputerNameExAType> = OnceLock::new();

type GetComputerNameExWType =
    unsafe extern "system" fn(name_type: u32, lpBuffer: *mut u16, nSize: *mut u32) -> BOOL;
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

type WSAIoctlType = unsafe extern "system" fn(
    s: SOCKET,
    dwIoControlCode: u32,
    lpvInBuffer: *mut c_void,
    cbInBuffer: u32,
    lpvOutBuffer: *mut c_void,
    cbOutBuffer: u32,
    lpcbBytesReturned: *mut u32,
    lpOverlapped: *mut WSAOVERLAPPED,
    lpCompletionRoutine: LPWSAOVERLAPPED_COMPLETION_ROUTINE,
) -> INT;
static WSA_IOCTL_ORIGINAL: OnceLock<&WSAIoctlType> = OnceLock::new();

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

type WSASocketWType = unsafe extern "system" fn(
    af: i32,
    socket_type: i32,
    protocol: i32,
    // LPWSAPROTOCOL_INFOW
    lpProtocolInfo: *mut u16,
    g: u32,
    dwFlags: u32,
) -> SOCKET;
static WSA_SOCKET_W_ORIGINAL: OnceLock<&WSASocketWType> = OnceLock::new();

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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
            register_windows_socket(socket, af, r#type, protocol);
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn bind_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("bind_detour -> socket: {}, namelen: {}", s, namelen);

    // Check if this socket is managed by mirrord using SocketManager
    if is_socket_managed(s) {
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

                    if result == ERROR_SUCCESS_I32 {
                        // Success - update socket state using SocketManager
                        let bound = Bound {
                            requested_address: bound_addr,
                            address: bound_addr,
                        };
                        set_socket_state(s, SocketState::Bound(bound));
                        tracing::info!(
                            "bind_detour -> socket {} bound through mirrord proxy to addr {}",
                            s,
                            bound_addr
                        );
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn listen_detour(s: SOCKET, backlog: INT) -> INT {
    tracing::trace!("listen_detour -> socket: {}, backlog: {}", s, backlog);

    // Check if this socket is managed by mirrord and get bound address
    if let Some(bind_addr) = get_bound_address(s) {
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

                if result == ERROR_SUCCESS_I32 {
                    // Success - update socket state to listening using SocketManager
                    if is_socket_in_state(s, |state| matches!(state, SocketState::Bound(_))) {
                        // Get the bound state and transition to listening
                        if let Some(SocketState::Bound(bound)) = get_socket_state(s) {
                            set_socket_state(s, SocketState::Listening(bound));
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
    } else {
        // Fall back to original function for non-managed sockets
    }

    let original = LISTEN_ORIGINAL.get().unwrap();
    unsafe { original(s, backlog) }
}

/// Windows socket hook for connect
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn connect_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("connect_detour -> socket: {}, namelen: {}", s, namelen);

    // Log whether this socket is managed and what kind
    if let Some(user_socket) = get_socket(s) {
        tracing::debug!(
            "connect_detour -> socket {} is managed, kind: {:?}",
            s,
            user_socket.kind
        );
    } else {
        tracing::debug!("connect_detour -> socket {} is not managed", s);
    }

    let socket_addr = match SocketAddr::try_from_raw(name, namelen) {
        Some(addr) => addr,
        None => {
            tracing::error!(
                "connect_detour -> failed to convert raw sockaddr for socket {}",
                s
            );
            return SOCKET_ERROR;
        }
    };
    let raw_addr = SockAddr::from(socket_addr);

    let connect_fn = |s: SocketDescriptor, addr: SockAddr| {
        let original = CONNECT_ORIGINAL.get().unwrap();
        let result = unsafe { original(s, addr.as_ptr() as *const _, addr.len()) };
        log_connection_result(result, "connect_detour", addr);
        ConnectResult::from(result)
    };

    match ops::attempt_proxy_connection(s, name, namelen, "connect_detour", connect_fn) {
        Err(HookError::ConnectError(ConnectError::AddressUnreachable(e))) => {
            tracing::error!(
                "connect_detour -> socket {} connect target {:?} is unreachable: {}",
                s,
                raw_addr,
                e
            );
            return SOCKET_ERROR;
        }
        Err(e) => {
            tracing::debug!(
                "connect_detour -> socket {} not managed, using original. err: {}",
                s,
                e
            );
        }
        Ok(connect_result) => {
            return connect_result.result();
        }
    }

    // fallback to original
    let original = CONNECT_ORIGINAL.get().unwrap();
    unsafe { original(s, name, namelen) }
}

/// Windows socket hook for accept
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
        if let Some(bound_addr) = get_bound_address(s) {
            // Check if listening socket is in listening state
            if is_socket_in_state(s, |state| matches!(state, SocketState::Listening(_))) {
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
                    if let Some(listening_socket) = get_socket(s) {
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn getsockname_detour(
    s: SOCKET,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> INT {
    tracing::trace!("getsockname_detour -> socket: {}", s);

    // Check if this socket is managed by mirrord and get its bound address
    if let Some(bound_addr) = get_bound_address(s) {
        match bound_addr.copy_to(name, namelen) {
            Ok(()) => {
                tracing::trace!(
                    "getsockname_detour -> returned mirrord bound address: {}",
                    bound_addr
                );

                return ERROR_SUCCESS_I32;
            }

            Err(err) => {
                tracing::debug!(
                    "getsockname_detour -> failed to convert bound address, error: {}",
                    err
                );

                match err {
                    WindowsError::WinSock(error_code) => unsafe {
                        WSASetLastError(error_code);
                    },

                    WindowsError::Windows(error_code) => {
                        tracing::warn!(
                            "getsockname_detour -> unexpected windows error converting bound address: error {}",
                            error_code
                        );
                    }
                };

                return SOCKET_ERROR;
            }
        }
    } else if let Some((_, local_addr, layer_addr)) = get_connected_addresses(s) {
        // Return the layer address for connected sockets if available, otherwise local address
        let addr_to_return = layer_addr.as_ref().unwrap_or(&local_addr);
        match addr_to_return.copy_to(name, namelen) {
            Ok(()) => {
                tracing::trace!("getsockname_detour -> returned mirrord local address");
                // Success
                return ERROR_SUCCESS_I32;
            }

            Err(err) => {
                tracing::debug!(
                    "getsockname_detour -> failed to convert layer address: {}",
                    err
                );

                match err {
                    WindowsError::WinSock(error_code) => unsafe {
                        WSASetLastError(error_code);
                    },

                    WindowsError::Windows(error_code) => {
                        tracing::warn!(
                            "getsockname_detour -> unexpected windows error converting layer address: error {}",
                            error_code
                        );
                    }
                };

                return SOCKET_ERROR;
            }
        }
    } else if is_socket_managed(s) {
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn getpeername_detour(
    s: SOCKET,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> INT {
    tracing::trace!("getpeername_detour -> socket: {}", s);

    // Check if this socket is managed and get connected addresses
    if let Some((remote_addr, _, _)) = get_connected_addresses(s) {
        // Return the remote address for connected sockets

        match remote_addr.copy_to(name, namelen) {
            Ok(()) => {
                tracing::trace!("getpeername_detour -> returned mirrord remote address");
                // Success
                return ERROR_SUCCESS_I32;
            }

            Err(err) => {
                tracing::debug!(
                    "getpeername_detour -> failed to convert remote address: {}",
                    err
                );

                match err {
                    WindowsError::WinSock(error_code) => unsafe {
                        WSASetLastError(error_code);
                    },

                    WindowsError::Windows(error_code) => {
                        tracing::warn!(
                            "getpeername_detour -> unexpected windows error converting remote address: error {}",
                            error_code
                        );
                    }
                };

                return SOCKET_ERROR;
            }
        }
    } else if is_socket_managed(s) {
        tracing::trace!(
            "getpeername_detour -> managed socket not in connected state, using original"
        );
    }

    // Fall back to original function for non-managed sockets or errors
    let original = GET_PEER_NAME_ORIGINAL.get().unwrap();
    unsafe { original(s, name, namelen) }
}

/// Pass-through hook for WSAStartup
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn wsa_startup_detour(wVersionRequested: u16, lpWSAData: *mut u8) -> i32 {
    tracing::debug!("WSAStartup called with version: {}", wVersionRequested);

    let original = WSA_STARTUP_ORIGINAL.get().unwrap();
    let result = unsafe { original(wVersionRequested, lpWSAData) };

    if result != ERROR_SUCCESS_I32 {
        tracing::warn!("WSAStartup failed with error: {}", result);
    }

    result
}

/// Pass-through hook for WSACleanup
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn wsa_cleanup_detour() -> i32 {
    // Pass through to original - let Windows Sockets handle cleanup
    let original = WSA_CLEANUP_ORIGINAL.get().unwrap();
    unsafe { original() }
}

/// Socket management detour for ioctlsocket() - controls I/O mode of socket
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn ioctlsocket_detour(s: SOCKET, cmd: i32, argp: *mut u32) -> i32 {
    // Pass through to original - interceptor handles I/O control for managed sockets
    let original = IOCTL_SOCKET_ORIGINAL.get().unwrap();
    unsafe { original(s, cmd, argp) }
}

/// Socket management detour for WSAIoctl - intercepts extension lookups
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn wsa_ioctl_detour(
    s: SOCKET,
    dwIoControlCode: u32,
    lpvInBuffer: *mut c_void,
    cbInBuffer: u32,
    lpvOutBuffer: *mut c_void,
    cbOutBuffer: u32,
    lpcbBytesReturned: *mut u32,
    lpOverlapped: *mut WSAOVERLAPPED,
    lpCompletionRoutine: LPWSAOVERLAPPED_COMPLETION_ROUTINE,
) -> INT {
    let original = WSA_IOCTL_ORIGINAL.get().unwrap();
    let result = unsafe {
        original(
            s,
            dwIoControlCode,
            lpvInBuffer,
            cbInBuffer,
            lpvOutBuffer,
            cbOutBuffer,
            lpcbBytesReturned,
            lpOverlapped,
            lpCompletionRoutine,
        )
    };

    if result == ERROR_SUCCESS_I32 && dwIoControlCode == SIO_GET_EXTENSION_FUNCTION_POINTER {
        unsafe {
            handle_connectex_extension_pointer(
                lpvInBuffer,
                cbInBuffer,
                lpvOutBuffer,
                cbOutBuffer,
                Some(connectex_detour),
            );
        }
    }

    result
}

/// Socket management detour for select() - monitors socket readiness
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn wsa_get_last_error_detour() -> i32 {
    let original = WSA_GET_LAST_ERROR_ORIGINAL.get().unwrap();
    let result = unsafe { original() };

    tracing::trace!("wsa_get_last_error_detour -> error: {}", result);

    result
}

/// Windows socket hook for WSASocket (advanced socket creation)
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
            register_windows_socket(socket, af, socket_type, protocol);
        }
    } else {
        tracing::warn!("wsa_socket_detour -> failed to create socket");
    }
    socket
}

#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn wsa_socket_w_detour(
    af: i32,
    socket_type: i32,
    protocol: i32,
    lpProtocolInfo: *mut u16,
    g: u32,
    dwFlags: u32,
) -> SOCKET {
    tracing::trace!(
        "wsa_socket_w_detour -> af: {}, type: {}, protocol: {}, flags: {}",
        af,
        socket_type,
        protocol,
        dwFlags
    );
    let original = WSA_SOCKET_W_ORIGINAL.get().unwrap();
    let socket = unsafe { original(af, socket_type, protocol, lpProtocolInfo, g, dwFlags) };
    if socket != INVALID_SOCKET {
        if af == AF_INET || af == AF_INET6 {
            register_windows_socket(socket, af, socket_type, protocol);
        }
    } else {
        tracing::warn!("wsa_socket_w_detour -> failed to create socket");
    }
    socket
}

/// Windows socket hook for ConnectEx (overlapped connect)
/// This function properly handles libuv's expectations for overlapped I/O completion
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn connectex_detour(
    s: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    lpSendBuffer: *mut c_void,
    dwSendDataLength: u32,
    lpdwBytesSent: *mut u32,
    lpOverlapped: *mut OVERLAPPED,
) -> BOOL {
    tracing::debug!(
        "connectex_detour -> socket: {}, namelen: {}, send_length: {}, overlapped: {:?}",
        s,
        namelen,
        dwSendDataLength,
        lpOverlapped
    );

    let original_connectex = match get_connectex_original() {
        Some(ptr) => ptr,
        None => {
            tracing::error!("connectex_detour -> original ConnectEx pointer not initialized");
            unsafe { WSASetLastError(WSAEFAULT) };
            return FALSE;
        }
    };

    let socket_addr = match SocketAddr::try_from_raw(name, namelen) {
        Some(addr) => addr,
        None => {
            tracing::error!(
                "connectex_detour -> failed to convert raw sockaddr for socket {}",
                s
            );
            unsafe { WSASetLastError(WSAEFAULT) };
            return FALSE;
        }
    };
    let raw_addr = SockAddr::from(socket_addr);

    // Check if this socket is managed by mirrord - if not, use original ConnectEx
    let is_managed = is_socket_managed(s);
    if !is_managed {
        tracing::debug!(
            "connectex_detour -> socket {} not managed, using original",
            s
        );
        let success = unsafe {
            original_connectex(
                s,
                raw_addr.as_ptr() as *const _,
                raw_addr.len(),
                lpSendBuffer,
                dwSendDataLength,
                lpdwBytesSent,
                lpOverlapped,
            )
        };

        let last_error = unsafe { WSAGetLastError() };
        tracing::debug!(
            "connectex_detour -> original result: success={}, last_error={}",
            success != FALSE,
            last_error
        );

        return success;
    }

    // For managed sockets, connect to the local proxy endpoint using original ConnectEx
    let connect_fn = |socket_descriptor: SocketDescriptor, addr: SockAddr| {
        tracing::debug!(
            "connectex_detour connect_fn -> establishing connection for socket {} to proxy at {:?}",
            socket_descriptor,
            addr
        );

        // Get the original ConnectEx function to connect to the proxy
        let original_connectex = match ops::get_connectex_original() {
            Some(func) => func,
            None => {
                tracing::error!("connectex_detour connect_fn -> original ConnectEx not available");
                return ConnectResult::new(SOCKET_ERROR, Some(WSAEFAULT));
            }
        };

        // Connect to the proxy address using original ConnectEx
        let result = unsafe {
            original_connectex(
                s,
                addr.as_ptr() as *const SOCKADDR,
                addr.len() as i32,
                lpSendBuffer,
                dwSendDataLength,
                lpdwBytesSent,
                lpOverlapped,
            )
        };

        let last_error = unsafe { WSAGetLastError() };

        tracing::debug!(
            "connectex_detour connect_fn -> original ConnectEx to proxy result: {}, last_error: {}",
            result,
            last_error
        );

        // Return the result from ConnectEx - layer-lib will handle the conversion
        if result != 0 {
            ConnectResult::new(ERROR_SUCCESS_I32, None)
        } else {
            ConnectResult::new(SOCKET_ERROR, Some(last_error))
        }
    };

    match ops::attempt_proxy_connection(s, name, namelen, "connectex_detour", connect_fn) {
        Err(HookError::ConnectError(ConnectError::AddressUnreachable(e))) => {
            tracing::error!(
                "connectex_detour -> socket {} connect target {:?} is unreachable: {}",
                s,
                raw_addr,
                e
            );
            unsafe { WSASetLastError(WSAEFAULT) };
            return FALSE;
        }
        Err(_) => {
            tracing::debug!(
                "connectex_detour -> socket {} not managed, using original",
                s
            );
            // Fall back to original ConnectEx
            let success = unsafe {
                original_connectex(
                    s,
                    raw_addr.as_ptr() as *const _,
                    raw_addr.len(),
                    lpSendBuffer,
                    dwSendDataLength,
                    lpdwBytesSent,
                    lpOverlapped,
                )
            };
            return success;
        }
        Ok(connect_result) => {
            tracing::debug!(
                "connectex_detour -> proxy connection result: {:?}",
                connect_result
            );

            // Handle the proxy connection result
            let error_opt = connect_result.error();
            let result_code: i32 = connect_result.into();
            tracing::debug!(
                "connectex_detour -> proxy connection result: {}",
                result_code
            );

            if result_code == ERROR_SUCCESS_I32 {
                return TRUE;
            } else if error_opt == Some(WSA_IO_PENDING) {
                // CRITICAL: For local proxy connections, WSA_IO_PENDING usually completes very
                // quickly Try to wait a short time for completion rather than
                // returning async
                tracing::info!(
                    "connectex_detour -> socket {} ConnectEx to proxy returned WSA_IO_PENDING, attempting immediate completion check",
                    s
                );

                // For now, return as async but with special handling
                unsafe {
                    WSASetLastError(WSA_IO_PENDING);
                }
                return FALSE;
            } else {
                // For async operations, set the last error and return FALSE
                if let Some(error) = error_opt {
                    unsafe {
                        WSASetLastError(error);
                    }
                    tracing::debug!(
                        "connectex_detour -> set last error to {} for async operation",
                        error
                    );
                }
                return FALSE;
            }
        }
    }
}

/// Windows socket hook for WSAConnect (asynchronous connect)
/// Node.js uses this for non-blocking connect operations
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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

    let connect_fn = |s: SocketDescriptor, addr: SockAddr| {
        // Call the original function with the prepared sockaddr
        let original = WSA_CONNECT_ORIGINAL.get().unwrap();
        let result = unsafe {
            original(
                s,
                addr.as_ptr() as *const _,
                addr.len(),
                lpCallerData,
                lpCalleeData,
                lpSQOS,
                lpGQOS,
            )
        };
        log_connection_result(result, "wsa_connect_detour", addr);
        ConnectResult::from(result)
    };

    let socket_addr = match SocketAddr::try_from_raw(name, namelen) {
        Some(addr) => addr,
        None => {
            tracing::error!(
                "wsa_connect_detour -> failed to convert raw sockaddr for socket {}",
                s
            );
            return SOCKET_ERROR;
        }
    };
    let raw_addr = SockAddr::from(socket_addr);

    match ops::attempt_proxy_connection(s, name, namelen, "wsa_connect_detour", connect_fn) {
        Err(HookError::ConnectError(ConnectError::AddressUnreachable(e))) => {
            tracing::error!(
                "wsa_connect_detour -> socket {} connect target {:?} is unreachable: {}",
                s,
                raw_addr,
                e
            );
            return SOCKET_ERROR;
        }
        Err(_) => {
            tracing::debug!(
                "wsa_connect_detour -> socket {} not managed, using original",
                s
            );
        }
        Ok(connect_result) => {
            return connect_result.result();
        }
    }

    // Fallback to original function
    let connect_res = connect_fn(s, raw_addr);
    connect_res.result()
}

/// Windows socket hook for WSAAccept (asynchronous accept)
/// Node.js uses this for non-blocking accept operations
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
        dwBufferCount,
    );

    // Helper function to consolidate all fallback calls to original WSASend
    let fallback_to_original = |reason: &str| {
        tracing::debug!("wsa_send_detour -> falling back to original: {}", reason);
        let original = WSA_SEND_ORIGINAL.get().unwrap();
        let res = unsafe {
            original(
                s,
                lpBuffers,
                dwBufferCount,
                lpNumberOfBytesSent,
                dwFlags,
                lpOverlapped,
                lpCompletionRoutine,
            )
        };
        tracing::debug!(
            "wsa_send_detour -> socket: {}, sentBytes: {}, result: {}, getlasterror: {}",
            s,
            unsafe { *lpNumberOfBytesSent },
            res,
            unsafe { WSAGetLastError() }
        );
        res
    };

    // Check if this socket is managed by mirrord
    let managed_socket = get_socket(s);

    if let Some(user_socket) = managed_socket {
        tracing::debug!(
            "wsa_send_detour -> socket {} is managed, kind: {:?}",
            s,
            user_socket.kind
        );

        // Check if outgoing traffic is enabled for this socket type
        let should_intercept = match user_socket.kind {
            SocketKind::Tcp(_) => {
                let tcp_outgoing = layer_setup().outgoing_config().tcp;
                tracing::debug!("wsa_send_detour -> TCP outgoing enabled: {}", tcp_outgoing);
                tcp_outgoing
            }
            SocketKind::Udp(_) => {
                let udp_outgoing = layer_setup().outgoing_config().udp;
                tracing::debug!("wsa_send_detour -> UDP outgoing enabled: {}", udp_outgoing);
                udp_outgoing
            }
        };

        if should_intercept {
            let socket_state =
                get_socket_state(s).map_or("Unknown".to_string(), |state| format!("{:?}", state));
            // Check if this socket is in connected state (proxy connection established)
            if is_socket_in_state(s, |state| matches!(state, SocketState::Connected(_))) {
                tracing::debug!(
                    "wsa_send_detour -> socket {} is connected via proxy, data will be routed through proxy",
                    s
                );

                // For proxy-connected sockets, the data routing happens automatically
                // through the proxy connection established in connect_detour/wsa_connect_detour
            } else {
                tracing::debug!(
                    "wsa_send_detour -> socket {} is managed but not connected ({}) via proxy, using original",
                    s,
                    socket_state
                );
            }
        } else {
            tracing::debug!(
                "wsa_send_detour -> outgoing traffic disabled for {:?}, using original",
                user_socket.kind
            );
        }
    }

    // Pass through to original - the proxy connection handles the routing if established
    fallback_to_original("expected - passing through to original")
}

/// Windows socket hook for WSARecv (asynchronous receive)
/// Node.js uses this extensively for overlapped I/O operations
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
/// This implementation uses the shared layer-lib sendto functionality to handle DNS resolution
/// and socket routing while preserving compatibility with Windows overlapped I/O.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
    tracing::debug!(
        "wsa_send_to_detour -> socket: {}, buffer_count: {}, to_len: {}",
        s,
        dwBufferCount,
        iTolen
    );

    let proxy_request_fn = |request| -> HookResult<_> {
        match make_proxy_request_with_response(request) {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
            Err(e) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
        }
    };

    // Helper function to consolidate all fallback calls to original WSASendTo
    let fallback_to_original = |reason: &str| {
        tracing::debug!("wsa_send_to_detour -> falling back to original: {}", reason);
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
    };

    // Handle overlapped I/O operations by falling back to original for async operations
    if !lpOverlapped.is_null() || !lpCompletionRoutine.is_null() {
        return fallback_to_original("overlapped I/O detected");
    }

    // For synchronous operations, we can use layer-lib functionality
    // Extract buffer data using our safe wrapper
    let buffer_data = match WSABufferData::try_from((lpBuffers, dwBufferCount)) {
        Ok(data) => data,
        Err(_) => {
            return fallback_to_original("invalid buffer parameters");
        }
    };

    tracing::debug!(
        "wsa_send_to_detour -> buffer_count: {}, total_length: {}, is_single: {}",
        buffer_data.buffer_count(),
        buffer_data.total_length(),
        buffer_data.is_single_buffer()
    );

    tracing::debug!("wsa_send_to_detour -> buffer: {:?}", buffer_data);

    // From Docs: (https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsasendto#remarks)
    // The WSASendTo function is normally used on a connectionless socket specified by s to send a
    // datagram contained in one or more buffers to a specific peer socket identified by the lpTo
    // parameter. Even if the connectionless socket has been previously connected using the
    // connect function to a specific address, lpTo overrides the destination address for that
    // particular datagram only. On a connection-oriented socket, the lpTo and iToLen parameters
    // are ignored; in this case, the WSASendTo is equivalent to WSASend.
    if lpTo.is_null() || iTolen <= 0 {
        // For connection-oriented sockets or when no destination is specified,
        // WSASendTo is equivalent to WSASend
        return unsafe {
            wsa_send_detour(
                s,
                lpBuffers,
                dwBufferCount,
                lpNumberOfBytesSent,
                dwFlags,
                lpOverlapped,
                lpCompletionRoutine,
            )
        };
    }

    // Convert Windows destination address to cross-platform format
    let raw_destination = match SocketAddr::try_from_raw(lpTo as *const _, iTolen as _) {
        Some(addr) => addr,
        None => unreachable!(),
    };

    // For simple single-buffer case, use layer-lib functionality
    if buffer_data.is_single_buffer() {
        let (first_buf_ptr, first_buf_len) = buffer_data.first_buffer().unwrap();

        // Windows WSASendTo function wrapper for layer-lib
        let wsa_sendto_fn = |sockfd: SOCKET,
                             buffer: *const u8,
                             length: usize,
                             send_flags: i32,
                             addr: SockAddr|
         -> HookResult<isize> {
            if lpNumberOfBytesSent.is_null() {
                return Err(HookError::NullPointer);
            }

            let original = WSA_SEND_TO_ORIGINAL.get().unwrap();

            // Create a WSABUF for the single buffer using our helper
            let wsabuf = buffer_data.create_single_wsabuf(buffer, length as u32);

            let mut bytes_sent = 0u32;
            let result = unsafe {
                original(
                    sockfd,
                    &wsabuf as *const _ as *mut u8,
                    1, // dwBufferCount
                    &mut bytes_sent,
                    send_flags as u32,
                    addr.as_ptr() as *const SOCKADDR,
                    addr.len() as INT,
                    std::ptr::null_mut(), // lpOverlapped
                    std::ptr::null_mut(), // lpCompletionRoutine
                )
            };

            if result == ERROR_SUCCESS_I32 {
                // Success - update bytes sent if caller provided pointer
                unsafe { *lpNumberOfBytesSent = bytes_sent };
                Ok(result.try_into().unwrap())
            } else {
                Err(SendToError::SendFailed(result.try_into().unwrap()).into())
            }
        };

        // Use the shared layer-lib sendto functionality
        match send_to(
            s,
            first_buf_ptr,
            first_buf_len as usize,
            dwFlags as i32,
            raw_destination,
            wsa_sendto_fn,
            proxy_request_fn,
        ) {
            Ok(sendto_result) => {
                tracing::debug!(
                    "wsa_send_to_detour -> layer-lib sendto success: {} bytes",
                    sendto_result
                );
                // WSASendTo returns 0 on success
                ERROR_SUCCESS_I32
            }
            Err(_) => {
                // On error, fall back to original function
                fallback_to_original("layer-lib sendto failed")
            }
        }
    } else {
        // For multi-buffer or complex cases, fall back to original function
        fallback_to_original(&format!(
            "multi-buffer case (count: {})",
            buffer_data.buffer_count()
        ))
    }
}

/// Windows socket hook for WSARecvFrom (asynchronous UDP receive)
/// Node.js uses this for overlapped UDP operations
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn gethostname_detour(name: *mut i8, namelen: INT) -> INT {
    tracing::debug!("gethostname_detour called with namelen: {}", namelen);
    let original = GET_HOST_NAME_ORIGINAL.get().unwrap();

    // IN namelen is not writable, as a workaround we work on local variable we'll just ditch
    let mut namelen_mut = namelen as u32;
    let namelen_ptr: *mut u32 = &mut namelen_mut;
    unsafe {
        // gethostname is similar to hostname_ansi except:
        //     * different ret vals
        //     * GLE (GetLastError) -> WSAGLE
        handle_hostname_ansi(
            name,
            namelen_ptr,
            || original(name, namelen),
            || get_remote_hostname(true),
            "gethostname",
            ERROR_BUFFER_OVERFLOW,
            // If no error occurs, gethostname returns zero. Otherwise, it returns SOCKET_ERROR
            //  and a specific error code can be retrieved by calling WSAGetLastError.
            (ERROR_SUCCESS_I32, SOCKET_ERROR),
        )
    }
}

/// Windows kernel32 hook for GetComputerNameA
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn get_computer_name_a_detour(lpBuffer: *mut i8, nSize: *mut u32) -> i32 {
    let original = GET_COMPUTER_NAME_A_ORIGINAL.get().unwrap();

    unsafe {
        handle_hostname_ansi(
            lpBuffer,
            nSize,
            || original(lpBuffer, nSize),
            || get_remote_hostname(true),
            "GetComputerNameA",
            ERROR_BUFFER_OVERFLOW,
            // If the function succeeds, the return value is a nonzero value.
            // If the function fails, the return value is zero.
            (1, 0),
        )
    }
}

/// Windows kernel32 hook for GetComputerNameW
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn get_computer_name_w_detour(lpBuffer: *mut u16, nSize: *mut u32) -> BOOL {
    let original = GET_COMPUTER_NAME_W_ORIGINAL.get().unwrap();
    unsafe {
        handle_hostname_unicode(
            lpBuffer,
            nSize,
            || original(lpBuffer, nSize),
            || get_remote_hostname(true),
            "GetComputerNameW",
            ERROR_BUFFER_OVERFLOW,
        )
    }
}

/// Windows kernel32 hook for GetComputerNameExA
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn get_computer_name_ex_a_detour(
    name_type: u32,
    lpBuffer: *mut i8,
    nSize: *mut u32,
) -> i32 {
    use winapi::um::sysinfoapi::*;

    tracing::debug!(
        "GetComputerNameExA hook called with name_type: {}",
        name_type
    );
    let original = GET_COMPUTER_NAME_EX_A_ORIGINAL.get().unwrap();

    // supported name types for hostname interception
    let should_intercept = matches!(
        name_type,
        ComputerNameDnsHostname
            | ComputerNameDnsFullyQualified
            | ComputerNamePhysicalDnsHostname
            | ComputerNamePhysicalDnsFullyQualified
            | ComputerNameNetBIOS
            | ComputerNamePhysicalNetBIOS
    );

    if should_intercept {
        return handle_hostname_ansi(
            lpBuffer,
            nSize,
            || unsafe { original(name_type, lpBuffer, nSize) },
            || hostname::get_hostname_for_name_type(name_type),
            "GetComputerNameExA",
            ERROR_MORE_DATA,
            // If the function succeeds, the return value is a nonzero value.
            // If the function fails, the return value is zero.
            (1, 0),
        );
    }

    // forward non-supported name_types to original func
    tracing::debug!(
        "GetComputerNameExW: unsupported name_type {}, falling back to original",
        name_type
    );
    return unsafe { original(name_type, lpBuffer, nSize) };
}

/// Windows kernel32 hook for GetComputerNameExW
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn get_computer_name_ex_w_detour(
    name_type: u32,
    lpBuffer: *mut u16,
    nSize: *mut u32,
) -> BOOL {
    use winapi::um::sysinfoapi::*;
    let original = GET_COMPUTER_NAME_EX_W_ORIGINAL.get().unwrap();

    // supported name types for hostname interception
    let should_intercept = matches!(
        name_type,
        ComputerNameDnsHostname
            | ComputerNameDnsFullyQualified
            | ComputerNamePhysicalDnsHostname
            | ComputerNamePhysicalDnsFullyQualified
            | ComputerNameNetBIOS
            | ComputerNamePhysicalNetBIOS
    );

    if should_intercept {
        return handle_hostname_unicode(
            lpBuffer,
            nSize,
            || unsafe { original(name_type, lpBuffer, nSize) },
            || hostname::get_hostname_for_name_type(name_type),
            "GetComputerNameExW",
            ERROR_MORE_DATA,
        );
    }

    // forward non-supported name_types to original func
    tracing::debug!(
        "GetComputerNameExW: unsupported name_type {}, falling back to original",
        name_type
    );
    return unsafe { original(name_type, lpBuffer, nSize) };
}

/// Hook for gethostbyname to handle DNS resolution of our modified hostname
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn gethostbyname_detour(name: *const i8) -> *mut HOSTENT {
    let fallback_to_original = || unsafe { GET_HOST_BY_NAME_ORIGINAL.get().unwrap()(name) };

    if name.is_null() {
        tracing::debug!("gethostbyname: name is null, calling original");
        return fallback_to_original();
    }

    // SAFETY: Validate the string pointer before dereferencing
    let hostname_cstr = match unsafe { std::ffi::CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            tracing::debug!("gethostbyname: invalid UTF-8 in hostname, calling original");
            return fallback_to_original();
        }
    };

    tracing::debug!("gethostbyname: resolving hostname: {}", hostname_cstr);

    // Check if this is our remote hostname
    if is_remote_hostname(hostname_cstr.to_string()) {
        tracing::debug!(
            "gethostbyname: intercepting resolution for our hostname: {}",
            hostname_cstr
        );
    }

    // Check if we should resolve this hostname remotely using the DNS selector
    let should_resolve_remotely = {
        let result = layer_setup()
            .dns_selector()
            .should_resolve_remotely(hostname_cstr, 0);
        tracing::debug!(
            "is_remote_hostname DNS selector check for '{}': {}",
            hostname_cstr,
            result
        );
        result
    };

    tracing::warn!(
        "DNS selector decision for {}: resolve_remotely={}",
        hostname_cstr,
        should_resolve_remotely
    );

    if !should_resolve_remotely {
        return fallback_to_original();
    }
    // Try to resolve the hostname using mirrord's remote DNS resolution
    match remote_dns_resolve_via_proxy(hostname_cstr) {
        Ok(results) => {
            if !results.is_empty() {
                // Use the first IP address from the results
                let (name, ip) = &results[0];
                tracing::debug!(
                    "Remote DNS resolution successful: {} -> {}",
                    hostname_cstr,
                    ip
                );

                // Create a proper HOSTENT structure from the resolved data using thread-local
                // storage This mimics WinSock's behavior where each thread has its
                // own HOSTENT buffer
                match create_thread_local_hostent(name.clone(), *ip) {
                    Ok(hostent_ptr) => return hostent_ptr,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to create HOSTENT structure for {}: {:?}",
                            hostname_cstr,
                            e
                        );
                        // Fall back to original function
                    }
                }
            } else {
                tracing::warn!(
                    "Remote DNS resolution returned empty results for {}",
                    hostname_cstr
                );
            }
            // fallback to original
        }
        Err(e) => {
            tracing::warn!("Remote DNS resolution failed for {}: {}", hostname_cstr, e);
            // fallback to original
        }
    }

    // For all other hostnames or if our hostname resolution fails, call original function
    tracing::debug!(
        "gethostbyname: calling original function for hostname: {}",
        hostname_cstr
    );
    return fallback_to_original();
}

/// Hook for getaddrinfo to handle DNS resolution with full mirrord functionality
///
/// This follows the same pattern as the Unix layer but uses Windows types and calling conventions.
/// It converts Windows ADDRINFOA structures and makes DNS requests through the mirrord agent.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn getaddrinfo_detour(
    raw_node: *const u8,
    raw_service: *const u8,
    raw_hints: *const ADDRINFOA,
    out_addr_info: *mut *mut ADDRINFOA,
) -> INT {
    let node_opt = match Option::from(raw_node) {
        Some(ptr) if !ptr.is_null() => {
            Some(unsafe { str_win::u8_buffer_to_string(PCSTR(ptr).as_bytes()) })
        }
        _ => None,
    };
    tracing::warn!("getaddrinfo_detour called for hostname: {:?}", node_opt);

    let service_opt = match Option::from(raw_service) {
        Some(ptr) if !ptr.is_null() => {
            Some(unsafe { str_win::u8_buffer_to_string(PCSTR(ptr).as_bytes()) })
        }
        _ => None,
    };

    let hints_ref = unsafe { raw_hints.as_ref() };

    unsafe {
        match windows_getaddrinfo::<ADDRINFOA>(node_opt, service_opt, hints_ref) {
            Ok(managed_addr_info) => {
                // Store the managed result pointer and move the object to MANAGED_ADDRINFO
                let addr_ptr = managed_addr_info.as_ptr();
                let mut managed = match MANAGED_ADDRINFO.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => {
                        tracing::warn!(
                            "getaddrinfo: MANAGED_ADDRINFO was poisoned, attempting recovery"
                        );
                        poisoned.into_inner()
                    }
                };
                managed.insert(addr_ptr as usize, ManagedAddrInfoAny::A(managed_addr_info));
                *out_addr_info = addr_ptr;
                // Success
                ERROR_SUCCESS_I32
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn getaddrinfow_detour(
    node_name: *const u16,
    service_name: *const u16,
    hints: *const ADDRINFOW,
    result: *mut *mut ADDRINFOW,
) -> INT {
    tracing::warn!("GetAddrInfoW_detour called");

    let node_opt = match Option::from(node_name) {
        Some(ptr) if !ptr.is_null() => unsafe {
            Some(str_win::u16_buffer_to_string(PCWSTR(ptr).as_wide()))
        },
        _ => None,
    };
    tracing::warn!("GetAddrInfoW_detour called for hostname: {:?}", node_opt);

    let service_opt = match Option::from(service_name) {
        Some(ptr) if !ptr.is_null() => unsafe {
            Some(str_win::u16_buffer_to_string(PCWSTR(ptr).as_wide()))
        },
        _ => None,
    };

    let hints_ref = unsafe { hints.as_ref() };

    // Use full windows_getaddrinfo approach with DNS selector logic and service/hints support
    match windows_getaddrinfo::<ADDRINFOW>(node_opt.clone(), service_opt, hints_ref) {
        Ok(managed_result) => {
            tracing::debug!("GetAddrInfoW: full resolution succeeded");
            // Store the managed result pointer and move the object to MANAGED_ADDRINFO
            let result_ptr = managed_result.as_ptr();
            unsafe { *result = result_ptr };
            let mut managed = match MANAGED_ADDRINFO.lock() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    tracing::warn!(
                        "GetAddrInfoW: MANAGED_ADDRINFO was poisoned, attempting recovery"
                    );
                    poisoned.into_inner()
                }
            };
            managed.insert(result_ptr as usize, ManagedAddrInfoAny::W(managed_result));
            return ERROR_SUCCESS_I32;
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn freeaddrinfo_t_detour(addrinfo: *mut ADDRINFOW) {
    unsafe {
        // note: supports both ADDRINFOA and ADDRINFOW,
        //  the proper dealloc will be called
        if !free_managed_addrinfo(addrinfo) {
            // Not one of ours - call original freeaddrinfo
            FREE_ADDR_INFO_W_ORIGINAL.get().unwrap()(addrinfo);
        }
    }
}

/// Data transfer detour for recv() - receives data from a socket
///
/// Note: For mirrord-managed outgoing connections, data flows automatically through
/// the interceptor. This detour just passes through to the original recv() which
/// operates on the socket connected to the interceptor.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn recv_detour(s: SOCKET, buf: *mut i8, len: INT, flags: INT) -> INT {
    // Pass through to original - interceptor handles data routing for managed sockets
    let original = RECV_ORIGINAL.get().unwrap();
    unsafe { original(s, buf, len, flags) }
}

/// Data transfer detour for send() - sends data to a socket
///
/// For mirrord-managed outgoing connections, this checks if the socket should be intercepted
/// and routes data through the proxy if outgoing traffic is enabled.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn send_detour(s: SOCKET, buf: *const i8, len: INT, flags: INT) -> INT {
    tracing::trace!("send_detour -> socket: {}, len: {}", s, len);

    // Check if this socket is managed by mirrord
    if let Some(user_socket) = get_socket(s) {
        tracing::debug!(
            "send_detour -> socket {} is managed, kind: {:?}",
            s,
            user_socket.kind
        );

        // Check if outgoing traffic is enabled for this socket type
        let should_intercept = match user_socket.kind {
            SocketKind::Tcp(_) => {
                let tcp_outgoing = layer_setup().outgoing_config().tcp;
                tracing::debug!("send_detour -> TCP outgoing enabled: {}", tcp_outgoing);
                tcp_outgoing
            }
            SocketKind::Udp(_) => {
                let udp_outgoing = layer_setup().outgoing_config().udp;
                tracing::debug!("send_detour -> UDP outgoing enabled: {}", udp_outgoing);
                udp_outgoing
            }
        };

        if should_intercept {
            // Check if this socket is in connected state (proxy connection established)
            if is_socket_in_state(s, |state| matches!(state, SocketState::Connected(_))) {
                tracing::debug!(
                    "send_detour -> socket {} is connected via proxy, data will be routed through proxy",
                    s
                );
                // For proxy-connected sockets, the data routing happens automatically
                // through the proxy connection established in connect_detour
            } else {
                tracing::debug!(
                    "send_detour -> socket {} is managed but not connected via proxy, using original",
                    s
                );
            }
        } else {
            tracing::debug!(
                "send_detour -> outgoing traffic disabled for {:?}, using original",
                user_socket.kind
            );
        }
    }

    // Pass through to original - the proxy connection handles the routing if established
    let original = SEND_ORIGINAL.get().unwrap();
    unsafe { original(s, buf, len, flags) }
}

/// Data transfer detour for recvfrom() - receives data from a socket with source address
///
/// Note: UDP/datagram sockets typically aren't managed by mirrord outgoing connections,
/// so this is primarily a pass-through for compatibility.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
/// This implementation uses the shared layer-lib sendto functionality to handle DNS resolution
/// and socket routing while preserving compatibility with Windows applications.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn sendto_detour(
    s: SOCKET,
    buf: *const i8,
    len: INT,
    flags: INT,
    to: *const SOCKADDR,
    tolen: INT,
) -> INT {
    tracing::debug!(
        "sendto_detour -> socket: {}, len: {}, tolen: {}",
        s,
        len,
        tolen
    );

    let proxy_request_fn = |request| match make_proxy_request_with_response(request) {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(e)) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
        Err(e) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
    };

    // Helper function to consolidate all fallback calls to original sendto
    let fallback_to_original = |reason: &str| -> INT {
        tracing::debug!("sendto_detour -> falling back to original: {}", reason);
        let original = SEND_TO_ORIGINAL.get().unwrap();
        unsafe { original(s, buf, len, flags, to, tolen) }
    };

    // Convert Windows parameters to cross-platform format
    let raw_destination = match SocketAddr::try_from_raw(to as *const _, tolen as _) {
        Some(addr) => addr,
        None => {
            return fallback_to_original("failed to parse destination address");
        }
    };

    // Windows sendto function wrapper
    let sendto_fn = |sockfd: SOCKET,
                     buffer: *const u8,
                     length: usize,
                     send_flags: i32,
                     addr: SockAddr|
     -> HookResult<isize> {
        let original = SEND_TO_ORIGINAL.get().unwrap();
        let result = unsafe {
            original(
                sockfd,
                buffer as *const i8,
                length as INT,
                send_flags,
                addr.as_ptr() as *const SOCKADDR,
                addr.len() as INT,
            )
        };
        Ok(result as isize)
    };

    // Use the shared layer-lib sendto functionality
    match send_to(
        s,
        buf as *const u8,
        len as usize,
        flags,
        raw_destination,
        sendto_fn,
        proxy_request_fn,
    ) {
        Ok(result) => {
            tracing::debug!(
                "sendto_detour -> layer-lib sendto success: {} bytes",
                result
            );
            result as INT
        }
        Err(e) => fallback_to_original(&format!("layer-lib error: {:?}", e)),
    }
}

/// Socket management detour for closesocket() - closes a socket
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn closesocket_detour(s: SOCKET) -> INT {
    let original = CLOSE_SOCKET_ORIGINAL.get().unwrap();
    let res = unsafe { original(s) };
    // Only clean up mirrord state if the close was successful
    if res == ERROR_SUCCESS_I32 {
        tracing::debug!(
            "closesocket_detour -> successfully closed socket {}, removing from mirrord tracking",
            s
        );
        remove_socket(s);
    } else {
        tracing::warn!(
            "closesocket_detour -> failed to close socket {}, not removing from mirrord tracking",
            s
        );
    }
    res
}

/// Socket management detour for shutdown() - shuts down part or all of a socket connection
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe extern "system" fn shutdown_detour(s: SOCKET, how: INT) -> INT {
    // Pass through to original - interceptor handles connection shutdown for managed sockets
    let original = SHUTDOWN_ORIGINAL.get().unwrap();
    unsafe { original(s, how) }
}

/// Socket option detour for setsockopt() - sets socket options
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
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
    let enabled_remote_dns = layer_setup().remote_dns_enabled();

    // Register core socket operations
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

    if enabled_remote_dns {
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

        apply_hook!(
            guard,
            "ws2_32",
            "getaddrinfo",
            getaddrinfo_detour,
            GetAddrInfoType,
            GET_ADDR_INFO_ORIGINAL
        )?;

        apply_hook!(
            guard,
            "ws2_32",
            "GetAddrInfoW",
            getaddrinfow_detour,
            GetAddrInfoWType,
            GET_ADDR_INFO_W_ORIGINAL
        )?;

        // FreeAddrInfoW is a fraud - don't need to detour it explictly
        // It should be named FreeAddrInfo(A|W), for reasons of UNICODE (blahblah) variable exports.
        // freeaddrinfo is the same func for freeaddrinfo and FreeAddrInfoW
        // MANAGED_ADDRINFO.remove called by freeaddrinfo_detour is aware of both cases
        // see: https://learn.microsoft.com/en-us/windows/win32/api/ws2def/ns-ws2def-addrinfow

        apply_hook!(
            guard,
            "ws2_32",
            "FreeAddrInfoW",
            freeaddrinfo_t_detour,
            FreeAddrInfoWType,
            FREE_ADDR_INFO_W_ORIGINAL
        )?;
    }

    apply_hook!(
        guard,
        "ws2_32",
        "gethostname",
        gethostname_detour,
        GetHostNameType,
        GET_HOST_NAME_ORIGINAL
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
        "WSAIoctl",
        wsa_ioctl_detour,
        WSAIoctlType,
        WSA_IOCTL_ORIGINAL
    )?;

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

    apply_hook!(
        guard,
        "ws2_32",
        "WSASocketA",
        wsa_socket_detour,
        WSASocketType,
        WSA_SOCKET_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "ws2_32",
        "WSASocketW",
        wsa_socket_w_detour,
        WSASocketWType,
        WSA_SOCKET_W_ORIGINAL
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

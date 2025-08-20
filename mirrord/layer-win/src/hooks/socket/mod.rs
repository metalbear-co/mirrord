//! Module responsible for registering hooks targeting socket operation syscalls.

use std::{
    collections::HashMap,
    ffi::CString,
    mem,
    ptr,
    sync::{LazyLock, Mutex, OnceLock},
};

use minhook_detours_rs::guard::DetourGuard;
use winapi::{
    shared::{
        minwindef::{INT},
        ws2def::{SOCKADDR, SOCKADDR_IN},
        ws2ipdef::{SOCKADDR_IN6},
    },
    um::{
        winsock2::{
            SOCKET, INVALID_SOCKET, SOCKET_ERROR, WSASetLastError,
        },
    },
};

use crate::apply_hook;

/// Holds the pair of IP addresses with their hostnames, resolved remotely.
/// This is the Windows equivalent of REMOTE_DNS_REVERSE_MAPPING.
static REMOTE_DNS_REVERSE_MAPPING: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Hostname initialized from the agent.
static HOSTNAME: LazyLock<Mutex<Option<CString>>> = LazyLock::new(|| Mutex::new(None));

/// Keep track of managed address info structures for proper cleanup.
static MANAGED_ADDRINFO: LazyLock<Mutex<std::collections::HashSet<usize>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

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

/// Windows socket hook for socket creation
unsafe extern "system" fn socket_detour(af: INT, r#type: INT, protocol: INT) -> SOCKET {
    tracing::trace!("socket_detour -> af: {}, type: {}, protocol: {}", af, r#type, protocol);
    
    // For now, just call the original function
    // TODO: Implement mirrord socket logic here
    let original = SOCKET_ORIGINAL.get().unwrap();
    unsafe {
        original(af, r#type, protocol)
    }
}

/// Windows socket hook for bind
unsafe extern "system" fn bind_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("bind_detour -> socket: {}, namelen: {}", s, namelen);
    
    // TODO: Implement mirrord bind logic here
    let original = BIND_ORIGINAL.get().unwrap();
    unsafe {
        original(s, name, namelen)
    }
}

/// Windows socket hook for listen
unsafe extern "system" fn listen_detour(s: SOCKET, backlog: INT) -> INT {
    tracing::trace!("listen_detour -> socket: {}, backlog: {}", s, backlog);
    
    // TODO: Implement mirrord listen logic here
    let original = LISTEN_ORIGINAL.get().unwrap();
    unsafe {
        original(s, backlog)
    }
}

/// Windows socket hook for connect
unsafe extern "system" fn connect_detour(s: SOCKET, name: *const SOCKADDR, namelen: INT) -> INT {
    tracing::trace!("connect_detour -> socket: {}, namelen: {}", s, namelen);
    
    // TODO: Implement mirrord connect logic here
    let original = CONNECT_ORIGINAL.get().unwrap();
    unsafe {
        original(s, name, namelen)
    }
}

/// Windows socket hook for accept
unsafe extern "system" fn accept_detour(s: SOCKET, addr: *mut SOCKADDR, addrlen: *mut INT) -> SOCKET {
    tracing::trace!("accept_detour -> socket: {}", s);
    
    // TODO: Implement mirrord accept logic here
    let original = ACCEPT_ORIGINAL.get().unwrap();
    let result = unsafe {
        original(s, addr, addrlen)
    };
    if result != INVALID_SOCKET {
        // TODO: Register this socket with mirrord layer
        tracing::trace!("accept_detour -> accepted socket: {}", result);
    }
    result
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
    tracing::trace!("gethostname_detour -> namelen: {}", namelen);
    
    // Check if we have a remote hostname
    if let Ok(hostname_guard) = HOSTNAME.lock() {
        if let Some(ref hostname) = *hostname_guard {
            let hostname_bytes = hostname.as_bytes_with_nul();
            if hostname_bytes.len() <= namelen as usize {
                unsafe {
                    ptr::copy_nonoverlapping(
                        hostname.as_ptr() as *const i8,
                        name,
                        hostname_bytes.len(),
                    );
                }
                return 0; // Success
            } else {
                unsafe {
                    WSASetLastError(10014); // WSAEFAULT
                }
                return SOCKET_ERROR;
            }
        }
    }
    
    // Fall back to original function
    let original = GETHOSTNAME_ORIGINAL.get().unwrap();
    unsafe {
        original(name, namelen)
    }
}

/// Initialize socket hooks by setting up detours for Windows socket functions
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
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

    Ok(())
}

/// Set the remote hostname that should be returned by gethostname
pub fn set_remote_hostname(hostname: String) -> Result<(), Box<dyn std::error::Error>> {
    let c_hostname = CString::new(hostname)?;
    if let Ok(mut hostname_guard) = HOSTNAME.lock() {
        *hostname_guard = Some(c_hostname);
        tracing::debug!("Set remote hostname for gethostname hook");
        Ok(())
    } else {
        Err("Failed to acquire hostname lock".into())
    }
}

/// Helper function to convert Windows SOCKADDR to a readable format for logging
unsafe fn sockaddr_to_string(addr: *const SOCKADDR, addr_len: INT) -> String {
    if addr.is_null() || addr_len < mem::size_of::<SOCKADDR>() as INT {
        return "invalid".to_string();
    }

    let family = unsafe { (*addr).sa_family };
    match family as u32 {
        2u32 => { // AF_INET
            if addr_len >= mem::size_of::<SOCKADDR_IN>() as INT {
                let addr_in = unsafe { &*(addr as *const SOCKADDR_IN) };
                let ip = u32::from_be(unsafe { *addr_in.sin_addr.S_un.S_addr() });
                let port = u16::from_be(addr_in.sin_port);
                format!("{}:{}", 
                    format!("{}.{}.{}.{}", 
                        (ip >> 24) & 0xFF, 
                        (ip >> 16) & 0xFF, 
                        (ip >> 8) & 0xFF, 
                        ip & 0xFF
                    ), 
                    port
                )
            } else {
                "invalid_ipv4".to_string()
            }
        }
        23u32 => { // AF_INET6
            if addr_len >= mem::size_of::<SOCKADDR_IN6>() as INT {
                // IPv6 address parsing would be more complex
                "ipv6_address".to_string()
            } else {
                "invalid_ipv6".to_string()
            }
        }
        _ => format!("unknown_family_{}", family),
    }
}
//! Utility functions for Windows socket operations
use std::{
    convert::TryFrom,
    ffi::CString,
    mem,
    net::{IpAddr, SocketAddr},
    ptr,
};

use mirrord_layer_lib::{
    error::{HookError, HookResult},
    socket::SocketAddrExt,
};
use winapi::{
    shared::{
        in6addr::IN6_ADDR,
        inaddr::IN_ADDR,
        minwindef::INT,
        winerror::ERROR_SUCCESS,
        ws2def::{AF_INET, AF_INET6, SOCKADDR, SOCKADDR_STORAGE},
    },
    um::winsock2::{
        HOSTENT, INVALID_SOCKET, SOCKET, SOCKET_ERROR, closesocket, getpeername, getsockname,
    },
};

pub const ERROR_SUCCESS_I32: i32 = ERROR_SUCCESS as i32;
const IPV4_ADDR_LEN: usize = mem::size_of::<IN_ADDR>();
const IPV6_ADDR_LEN: usize = mem::size_of::<IN6_ADDR>();

/// RAII wrapper for automatically closing sockets on error
/// The socket will be automatically closed when dropped unless explicitly released
pub struct AutoCloseSocket {
    socket: SOCKET,
    should_close: bool,
}

impl AutoCloseSocket {
    pub fn new(socket: SOCKET) -> Self {
        Self {
            socket,
            should_close: true,
        }
    }

    pub fn get(&self) -> SOCKET {
        self.socket
    }

    /// Release the socket from automatic cleanup (call this on success)
    pub fn release(mut self) -> SOCKET {
        self.should_close = false;
        self.socket
    }
}

impl Drop for AutoCloseSocket {
    fn drop(&mut self) {
        if self.should_close && self.socket != INVALID_SOCKET {
            // Use WinAPI directly to close the socket to avoid circular dependencies
            unsafe {
                closesocket(self.socket);
                tracing::debug!(
                    "AutoCloseSocket -> automatically closed socket {}",
                    self.socket
                );
            }
        }
    }
}

thread_local! {
    /// Thread-local storage for HOSTENT structures to mimic WinSock behavior
    static THREAD_HOSTENT: std::cell::RefCell<Option<ManagedHostent>> = const { std::cell::RefCell::new(None) };
}

/// enum for owning IPADDR_IN and IPADDR6_IN data and cleanup on drop
/// it is only read as ptr, therefore clippy warns as dead_code
#[allow(dead_code)]
enum HostentAddrBuf {
    V4(Box<[u8; IPV4_ADDR_LEN]>),
    V6(Box<[u8; IPV6_ADDR_LEN]>),
}

/// RAII wrapper for Windows HOSTENT structure that automatically cleans up on drop
pub struct ManagedHostent {
    hostent: Box<HOSTENT>,

    // the data's ptrs are encompassed in hostent,
    //  we hold the data itself to own it and cleanup on drop
    //  clippy doesnt consider this is as read so it is named _
    _data: Box<ManagedHostentData>,
}

pub struct ManagedHostentData {
    hostname: CString,
    aliases_ptrs: Vec<*mut i8>,
    _addr_buf: HostentAddrBuf,
    addr_list: Vec<*mut i8>,
}

// SAFETY: We ensure thread safety by only accessing the pointer through controlled operations
// and the pointer is only used for memory management within the same process
unsafe impl Send for ManagedHostent {}
unsafe impl Sync for ManagedHostent {}

impl ManagedHostent {
    /// Get the raw pointer (for returning to Windows API)
    pub fn as_ptr(&self) -> *mut HOSTENT {
        self.hostent.as_ref() as *const HOSTENT as *mut HOSTENT
    }
}

impl TryFrom<(String, IpAddr)> for ManagedHostent {
    type Error = HookError;

    /// Creates a WinAPI compliant HOSTENT structure from hostname and IP address.
    fn try_from((hostname, ip): (String, IpAddr)) -> HookResult<Self> {
        let hostname = CString::new(hostname)?;

        // Note from WINAPI: h_aliases is a NULL-terminated list of alternate names
        // produce empty aliases ptr list (we having nothing but the hostname)
        // So if we ever want to support aliases, we will need to own them in ManagedHostent, in
        // addition to their reported pointer
        let mut aliases_ptrs: Vec<*mut i8> = Vec::with_capacity(1);
        aliases_ptrs.push(ptr::null_mut());

        let (addr_buf, addr_ptr, addrtype, addrlen) = match ip {
            IpAddr::V4(ipv4) => {
                let boxed = Box::new(ipv4.octets());
                let ptr = boxed.as_ref().as_ptr() as *mut u8;
                (
                    HostentAddrBuf::V4(boxed),
                    ptr,
                    AF_INET as i16,
                    IPV4_ADDR_LEN as i16,
                )
            }
            IpAddr::V6(ipv6) => {
                let boxed = Box::new(ipv6.octets());
                let ptr = boxed.as_ref().as_ptr() as *mut u8;
                (
                    HostentAddrBuf::V6(boxed),
                    ptr,
                    AF_INET6 as i16,
                    IPV6_ADDR_LEN as i16,
                )
            }
        };

        // Note from WINAPI: addr_list is a NULL-terminated list of addresses for the host
        // we were given one address, so we hold an address list of 1 + null termination pointer
        let mut addr_list: Vec<*mut i8> = vec![ptr::null_mut(); 2];
        addr_list[0] = addr_ptr as *mut i8;

        let mut data = Box::new(ManagedHostentData {
            hostname,
            aliases_ptrs,
            _addr_buf: addr_buf,
            addr_list,
        });

        let hostent = Box::new(HOSTENT {
            h_name: data.hostname.as_ptr() as *mut i8,
            h_aliases: data.aliases_ptrs.as_mut_ptr(),
            h_addrtype: addrtype,
            h_length: addrlen,
            h_addr_list: data.addr_list.as_mut_ptr(),
        });

        Ok(Self {
            hostent,
            _data: data,
        })
    }
}

/// Creates a WinSock-compliant HOSTENT structure using thread-local storage
///
/// This function mimics the behavior of WinSock's gethostbyname function by:
/// - Using thread-local storage that persists across calls on the same thread
/// - Automatically cleaning up when the thread exits
/// - Reusing the same memory location on subsequent calls (overwriting previous results)
/// - Returning a raw pointer that remains valid until the next call or thread exit
///
/// # Arguments
/// * `hostname` - The hostname string
/// * `ip` - The IP address (IPv4 or IPv6)
///
/// # Returns
/// * `Ok(*mut HOSTENT)` - Raw pointer to thread-local HOSTENT structure
/// * `Err(HookError)` - If allocation fails
///
/// # Safety
/// The returned pointer is valid until:
/// 1. The next call to this function on the same thread (overwrites the data)
/// 2. The thread exits (automatic cleanup)
///
/// This matches the documented behavior of WinSock's gethostbyname function.
pub fn create_thread_local_hostent(hostname: String, ip: IpAddr) -> HookResult<*mut HOSTENT> {
    THREAD_HOSTENT.with(|cell| {
        let mut hostent_ref = cell.borrow_mut();

        // Create new ManagedHostent (this will replace any existing one, mimicking WinSock
        // behavior)
        let new_hostent = ManagedHostent::try_from((hostname, ip))?;
        let ptr = new_hostent.as_ptr();

        // Store in thread-local storage, replacing any previous value
        // The old ManagedHostent will be dropped automatically, cleaning up its memory
        *hostent_ref = Some(new_hostent);

        Ok(ptr)
    })
}

/// Helper function to validate input parameters for Windows API functions
///
/// This function provides common validation logic for Windows API functions that take
/// buffer pointers and size parameters, helping to prevent buffer overflows and other
/// security issues.
pub fn validate_buffer_params(
    _buffer: *mut u8,
    size: *mut u32,
    max_reasonable_size: usize,
) -> Option<usize> {
    if size.is_null() {
        return None;
    }

    let buffer_size = unsafe { *size } as usize;
    if buffer_size == 0 {
        tracing::warn!("Empty buffer passed");
        return None;
    }

    if buffer_size > max_reasonable_size {
        tracing::warn!("Suspicious buffer size {}, rejecting request", buffer_size);
        return None;
    }

    Some(buffer_size)
}

/// Get the peer address from a connected socket
#[allow(clippy::result_large_err)]
pub fn get_peer_address_from_socket(socket: SOCKET) -> HookResult<SocketAddr> {
    let mut addr_storage: SOCKADDR_STORAGE = unsafe { mem::zeroed() };
    let mut addr_len = mem::size_of::<SOCKADDR_STORAGE>() as INT;

    let result = unsafe {
        getpeername(
            socket,
            &mut addr_storage as *mut _ as *mut SOCKADDR,
            &mut addr_len,
        )
    };

    if result == SOCKET_ERROR {
        return Err(HookError::IO(std::io::Error::last_os_error()));
    }

    unsafe { SocketAddr::try_from_raw(&addr_storage as *const _ as *const SOCKADDR, addr_len) }
        .ok_or_else(|| HookError::IO(std::io::Error::last_os_error()))
}

/// Helper function to get the actual bound address from a socket
pub unsafe fn get_actual_bound_address(socket: SOCKET, requested_addr: SocketAddr) -> SocketAddr {
    let mut actual_addr_storage: SOCKADDR_STORAGE = unsafe { std::mem::zeroed() };
    let mut actual_addr_len = std::mem::size_of::<SOCKADDR_STORAGE>() as INT;

    let getsockname_result = unsafe {
        getsockname(
            socket,
            &mut actual_addr_storage as *mut _ as *mut SOCKADDR,
            &mut actual_addr_len,
        )
    };

    if getsockname_result == ERROR_SUCCESS_I32 {
        match unsafe {
            SocketAddr::try_from_raw(
                &actual_addr_storage as *const _ as *const SOCKADDR,
                actual_addr_len,
            )
        } {
            Some(addr) => addr,
            None => {
                tracing::error!(
                    "get_actual_bound_address -> failed to convert actual bound address"
                );
                requested_addr
            }
        }
    } else {
        tracing::error!("get_actual_bound_address -> getsockname failed");
        requested_addr
    }
}

/// Helper function to determine the appropriate local binding address
pub fn determine_local_address(requested_addr: SocketAddr) -> SocketAddr {
    if requested_addr.ip().is_loopback() || requested_addr.ip().is_unspecified() {
        requested_addr
    } else if requested_addr.is_ipv4() {
        SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            requested_addr.port(),
        )
    } else {
        SocketAddr::new(
            std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
            requested_addr.port(),
        )
    }
}

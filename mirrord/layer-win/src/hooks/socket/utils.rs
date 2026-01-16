//! Utility functions for Windows socket operations
use std::{
    alloc::Layout,
    convert::TryFrom,
    ffi::CString,
    mem,
    net::{IpAddr, SocketAddr},
    ptr,
};

use mirrord_layer_lib::{
    error::{AddrInfoError, HookError, HookResult},
    socket::SocketAddrExt,
    unsafe_alloc,
};
use winapi::{
    shared::{
        minwindef::INT,
        winerror::ERROR_SUCCESS,
        ws2def::{AF_INET, AF_INET6, SOCKADDR, SOCKADDR_STORAGE},
    },
    um::winsock2::{
        HOSTENT, INVALID_SOCKET, SOCKET, SOCKET_ERROR, closesocket, getpeername, getsockname,
    },
};

pub const ERROR_SUCCESS_I32: i32 = ERROR_SUCCESS as i32;

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

/// RAII wrapper for Windows HOSTENT structure that automatically cleans up on drop
#[derive(Debug)]
pub struct ManagedHostent {
    ptr: *mut HOSTENT,
}

// SAFETY: We ensure thread safety by only accessing the pointer through controlled operations
// and the pointer is only used for memory management within the same process
unsafe impl Send for ManagedHostent {}
unsafe impl Sync for ManagedHostent {}

impl ManagedHostent {
    /// Get the raw pointer (for returning to Windows API)
    pub fn as_ptr(&self) -> *mut HOSTENT {
        self.ptr
    }

    /// Create a new managed HOSTENT from a raw pointer
    ///
    /// # Safety
    /// The caller must ensure the pointer is valid and was allocated by our system
    pub unsafe fn new(ptr: *mut HOSTENT) -> Self {
        Self { ptr }
    }
}

impl TryFrom<(String, IpAddr)> for ManagedHostent {
    type Error = AddrInfoError;

    /// Creates a WinAPI compliant HOSTENT structure from hostname and IP address
    ///
    /// This function properly allocates all required memory using the unsafe_alloc macro
    /// and follows WinAPI requirements:
    /// - All strings and arrays are heap-allocated
    /// - Arrays are null-terminated
    /// - Address type and length match the IP version
    /// - Memory remains valid until explicitly freed
    ///
    /// # Arguments
    /// * `value` - A tuple containing (hostname, ip_address)
    ///
    /// # Returns
    /// * `Ok(ManagedHostent)` - Managed HOSTENT structure
    /// * `Err(AddrInfoError)` - If allocation fails
    fn try_from((hostname, ip): (String, IpAddr)) -> Result<Self, Self::Error> {
        // Allocate the main HOSTENT structure
        let hostent_ptr = unsafe_alloc!(HOSTENT, AddrInfoError::AllocationFailed)?;

        // Initialize the structure with null values
        unsafe {
            ptr::write(
                hostent_ptr,
                HOSTENT {
                    h_name: ptr::null_mut(),
                    h_aliases: ptr::null_mut(),
                    h_addrtype: 0,
                    h_length: 0,
                    h_addr_list: ptr::null_mut(),
                },
            );
        }

        // Set the hostname - allocate and copy the string
        let hostname_cstring = match CString::new(hostname) {
            Ok(cstr) => cstr,
            Err(_) => {
                // Clean up the already allocated HOSTENT
                unsafe {
                    std::alloc::dealloc(hostent_ptr as *mut u8, Layout::new::<HOSTENT>());
                }
                return Err(AddrInfoError::AllocationFailed);
            }
        };

        // Allocate memory for the hostname string
        let hostname_bytes = hostname_cstring.into_bytes_with_nul();
        let hostname_layout = Layout::from_size_align(hostname_bytes.len(), 1)
            .map_err(|_| AddrInfoError::AllocationFailed)?;
        let hostname_ptr = unsafe { std::alloc::alloc(hostname_layout) as *mut i8 };
        if hostname_ptr.is_null() {
            unsafe {
                std::alloc::dealloc(hostent_ptr as *mut u8, Layout::new::<HOSTENT>());
            }
            return Err(AddrInfoError::AllocationFailed);
        }

        // Copy hostname data
        unsafe {
            ptr::copy_nonoverlapping(
                hostname_bytes.as_ptr(),
                hostname_ptr as *mut u8,
                hostname_bytes.len(),
            );
            (*hostent_ptr).h_name = hostname_ptr;
        }

        // Create empty aliases array (just null terminator)
        let aliases_ptr = unsafe_alloc!(*mut i8, AddrInfoError::AllocationFailed)?;
        unsafe {
            ptr::write(aliases_ptr, ptr::null_mut());
            (*hostent_ptr).h_aliases = aliases_ptr;
        }

        match ip {
            IpAddr::V4(ipv4) => {
                unsafe {
                    (*hostent_ptr).h_addrtype = AF_INET as i16;
                    (*hostent_ptr).h_length = 4;
                }

                // Allocate memory for IPv4 address (4 bytes)
                let addr_ptr = unsafe_alloc!([u8; 4], AddrInfoError::AllocationFailed)?;
                let addr_bytes = ipv4.octets();
                unsafe {
                    ptr::write(addr_ptr, addr_bytes);
                }

                // Create address list array with one address + null terminator
                let addr_list_ptr = unsafe_alloc!([*mut i8; 2], AddrInfoError::AllocationFailed)?;
                unsafe {
                    ptr::write(addr_list_ptr, [addr_ptr as *mut i8, ptr::null_mut()]);
                    (*hostent_ptr).h_addr_list = addr_list_ptr as *mut *mut i8;
                }
            }
            IpAddr::V6(ipv6) => {
                unsafe {
                    (*hostent_ptr).h_addrtype = AF_INET6 as i16;
                    (*hostent_ptr).h_length = 16;
                }

                // Allocate memory for IPv6 address (16 bytes)
                let addr_ptr = unsafe_alloc!([u8; 16], AddrInfoError::AllocationFailed)?;
                let addr_bytes = ipv6.octets();
                unsafe {
                    ptr::write(addr_ptr, addr_bytes);
                }

                // Create address list array with one address + null terminator
                let addr_list_ptr = unsafe_alloc!([*mut i8; 2], AddrInfoError::AllocationFailed)?;
                unsafe {
                    ptr::write(addr_list_ptr, [addr_ptr as *mut i8, ptr::null_mut()]);
                    (*hostent_ptr).h_addr_list = addr_list_ptr as *mut *mut i8;
                }
            }
        }

        Ok(unsafe { Self::new(hostent_ptr) })
    }
}

impl Drop for ManagedHostent {
    /// Safely frees all memory associated with a HOSTENT structure
    ///
    /// This function properly deallocates all the memory that was allocated in the TryFrom
    /// implementation:
    /// - The hostname string
    /// - The aliases array
    /// - The address data
    /// - The address list array
    /// - The HOSTENT structure itself
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }

        let hostent = unsafe { &*self.ptr };

        // Free hostname string
        if !hostent.h_name.is_null() {
            // Calculate the length of the null-terminated string
            let hostname_len = unsafe {
                let ptr = hostent.h_name;
                (0..).take_while(|&i| *ptr.offset(i) != 0).count() + 1
            };

            if let Ok(layout) = Layout::from_size_align(hostname_len, 1) {
                unsafe {
                    std::alloc::dealloc(hostent.h_name as *mut u8, layout);
                }
            }
        }

        // Free aliases array (we only allocated space for one null pointer)
        if !hostent.h_aliases.is_null() {
            unsafe {
                std::alloc::dealloc(hostent.h_aliases as *mut u8, Layout::new::<*mut i8>());
            }
        }

        // Free address list and address data
        if !hostent.h_addr_list.is_null() {
            let first_addr = unsafe { *hostent.h_addr_list };
            if !first_addr.is_null() {
                // Free the address data based on the address type
                match hostent.h_addrtype as i32 {
                    AF_INET => unsafe {
                        std::alloc::dealloc(first_addr as *mut u8, Layout::new::<[u8; 4]>());
                    },
                    AF_INET6 => unsafe {
                        std::alloc::dealloc(first_addr as *mut u8, Layout::new::<[u8; 16]>());
                    },
                    _ => {} // Unknown address type, skip deallocation to avoid corruption
                }
            }

            // Free the address list array (contains 2 pointers: address + null)
            unsafe {
                std::alloc::dealloc(
                    hostent.h_addr_list as *mut u8,
                    Layout::new::<[*mut i8; 2]>(),
                );
            }
        }

        // Finally, free the HOSTENT structure itself
        unsafe {
            std::alloc::dealloc(self.ptr as *mut u8, Layout::new::<HOSTENT>());
        }
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
/// * `Err(AddrInfoError)` - If allocation fails
///
/// # Safety
/// The returned pointer is valid until:
/// 1. The next call to this function on the same thread (overwrites the data)
/// 2. The thread exits (automatic cleanup)
///
/// This matches the documented behavior of WinSock's gethostbyname function.
pub fn create_thread_local_hostent(
    hostname: String,
    ip: IpAddr,
) -> Result<*mut HOSTENT, AddrInfoError> {
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

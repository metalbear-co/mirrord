//! Utility functions for Windows socket operations
use std::{
    alloc::Layout,
    convert::TryFrom,
    ffi::CString,
    mem,
    net::{IpAddr, SocketAddr},
    ptr,
};

use mirrord_layer_lib::error::{
    AddrInfoError, HookError, HookResult,
    windows::{WindowsError, WindowsResult},
};
use mirrord_protocol::{
    dns::GetAddrInfoResponse,
    error::{DnsLookupError, ResolveErrorKindInternal, ResponseError},
    outgoing::SocketAddress,
};
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{
            ADDRINFOA, ADDRINFOW, AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN, SOCKADDR_STORAGE,
        },
        ws2ipdef::SOCKADDR_IN6,
    },
    um::winsock2::{HOSTENT, SOCK_STREAM, WSAEFAULT},
};

/// Macro to safely allocate memory for Windows structures with error handling
#[macro_export]
macro_rules! unsafe_alloc {
    ($type:ty, $err: expr) => {{
        let layout = std::alloc::Layout::new::<$type>();
        let ptr = unsafe { std::alloc::alloc(layout) as *mut $type };
        if ptr.is_null() { Err($err) } else { Ok(ptr) }
    }};
}

thread_local! {
    /// Thread-local storage for HOSTENT structures to mimic WinSock behavior
    static THREAD_HOSTENT: std::cell::RefCell<Option<ManagedHostent>> = const { std::cell::RefCell::new(None) };
}

/// Windows-specific extensions for socket address handling on Windows
pub trait SocketAddrExtWin {
    /// Converts a raw Windows SOCKADDR pointer into a more _Rusty_ type
    fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<Self>
    where
        Self: Sized;

    /// Copies the socket address data into a Windows SOCKADDR structure for API calls
    fn copy_to(&self, name: *mut SOCKADDR, namelen: *mut INT) -> WindowsResult<()>;

    /// Creates an owned SOCKADDR representation of this address
    fn to_sockaddr(&self) -> WindowsResult<(SOCKADDR_STORAGE, INT)> {
        let mut storage: SOCKADDR_STORAGE = unsafe { mem::zeroed() };
        let mut len = mem::size_of::<SOCKADDR_STORAGE>() as INT;
        self.copy_to(&mut storage as *mut _ as *mut SOCKADDR, &mut len)?;
        Ok((storage, len))
    }
}

impl SocketAddrExtWin for SocketAddr {
    fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<SocketAddr> {
        unsafe { sockaddr_to_socket_addr(raw_address, address_length) }
    }
    fn copy_to(&self, name: *mut SOCKADDR, namelen: *mut INT) -> WindowsResult<()> {
        unsafe { socketaddr_to_windows_sockaddr(self, name, namelen) }
    }
}

impl SocketAddrExtWin for SocketAddress {
    fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<Self> {
        SocketAddr::try_from_raw(raw_address, address_length).map(SocketAddress::from)
    }
    fn copy_to(&self, name: *mut SOCKADDR, namelen: *mut INT) -> WindowsResult<()> {
        let std_addr =
            SocketAddr::try_from(self.clone()).map_err(|_| WindowsError::WinSock(WSAEFAULT))?;
        std_addr.copy_to(name, namelen)
    }
    fn to_sockaddr(&self) -> WindowsResult<(SOCKADDR_STORAGE, INT)> {
        let std_addr =
            SocketAddr::try_from(self.clone()).map_err(|_| WindowsError::WinSock(WSAEFAULT))?;
        std_addr.to_sockaddr()
    }
}
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
            let ip =
                unsafe { std::net::Ipv4Addr::from(u32::from_be(*addr_in.sin_addr.S_un.S_addr())) };
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

/// Convert SocketAddr to Windows SOCKADDR for address return functions
pub unsafe fn socketaddr_to_windows_sockaddr(
    addr: &SocketAddr,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> WindowsResult<()> {
    if name.is_null() || namelen.is_null() {
        return Err(WindowsError::WinSock(WSAEFAULT));
    }

    let name_len_value = unsafe { *namelen };
    if name_len_value < 0 {
        return Err(WindowsError::WinSock(WSAEFAULT));
    }

    match addr {
        SocketAddr::V4(addr_v4) => {
            let size = mem::size_of::<SOCKADDR_IN>() as INT;
            if name_len_value < size {
                return Err(WindowsError::WinSock(WSAEFAULT));
            }

            let mut sockaddr_in: SOCKADDR_IN = unsafe { mem::zeroed() };
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
        SocketAddr::V6(addr_v6) => {
            let size = mem::size_of::<SOCKADDR_IN6>() as INT;
            if name_len_value < size {
                return Err(WindowsError::WinSock(WSAEFAULT));
            }

            let mut sockaddr_in6: SOCKADDR_IN6 = unsafe { mem::zeroed() };
            sockaddr_in6.sin6_family = AF_INET6 as u16;
            sockaddr_in6.sin6_port = addr_v6.port().to_be();
            sockaddr_in6.sin6_flowinfo = addr_v6.flowinfo();
            // Note: scope_id is not available in SOCKADDR_IN6_LH

            // Copy the IPv6 address bytes
            let ip_bytes = addr_v6.ip().octets();
            unsafe {
                let bytes_ptr = sockaddr_in6.sin6_addr.u.Byte().as_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(ip_bytes.as_ptr(), bytes_ptr, 16);
            }

            unsafe {
                std::ptr::copy_nonoverlapping(
                    &sockaddr_in6 as *const _ as *const u8,
                    name as *mut u8,
                    size as usize,
                );
                *namelen = size;
            }

            Ok(())
        }
    }
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

/// Trait to abstract over Windows ADDRINFO types (ADDRINFOA and ADDRINFOW)
pub trait WindowsAddrInfo: Sized {
    type CanonName;

    /// Allocate a new instance
    unsafe fn alloc() -> Result<*mut Self, AddrInfoError>;

    /// Fill the structure with the given parameters
    unsafe fn fill(
        ptr: *mut Self,
        flags: i32,
        family: i32,
        socktype: i32,
        protocol: i32,
        addrlen: usize,
        canonname: Self::CanonName,
        addr: *mut SOCKADDR,
        next: *mut Self,
    );

    /// Set the next pointer
    unsafe fn set_next(ptr: *mut Self, next: *mut Self);

    /// Convert a string to the appropriate canonical name type
    fn string_to_canonname(s: String) -> Result<Self::CanonName, AddrInfoError>;

    /// Get null canonical name
    fn null_canonname() -> Self::CanonName;

    /// Extract family, socktype, and protocol from this structure (for hints processing)
    fn get_family_socktype_protocol(&self) -> (i32, i32, i32);

    // Field accessor methods for Drop implementation
    /// Get the ai_next field
    unsafe fn ai_next(&self) -> *mut Self;

    /// Get the ai_addr field
    unsafe fn ai_addr(&self) -> *mut SOCKADDR;

    /// Get the ai_family field
    unsafe fn ai_family(&self) -> i32;

    /// Get the ai_canonname field
    unsafe fn ai_canonname(&self) -> Self::CanonName;

    /// Free the canonical name
    unsafe fn free_canonname(canonname: Self::CanonName);
}

impl WindowsAddrInfo for ADDRINFOA {
    type CanonName = *mut i8;

    unsafe fn alloc() -> Result<*mut Self, AddrInfoError> {
        unsafe_alloc!(ADDRINFOA, AddrInfoError::AllocationFailed)
    }

    unsafe fn fill(
        ptr: *mut Self,
        flags: i32,
        family: i32,
        socktype: i32,
        protocol: i32,
        addrlen: usize,
        canonname: Self::CanonName,
        addr: *mut SOCKADDR,
        next: *mut Self,
    ) {
        unsafe {
            (*ptr).ai_flags = flags;
            (*ptr).ai_family = family;
            (*ptr).ai_socktype = socktype;
            (*ptr).ai_protocol = protocol;
            (*ptr).ai_addrlen = addrlen;
            (*ptr).ai_canonname = canonname;
            (*ptr).ai_addr = addr;
            (*ptr).ai_next = next;
        }
    }

    unsafe fn set_next(ptr: *mut Self, next: *mut Self) {
        unsafe {
            (*ptr).ai_next = next;
        }
    }

    fn string_to_canonname(s: String) -> Result<Self::CanonName, AddrInfoError> {
        use std::ffi::CString;
        match CString::new(s) {
            Ok(cstr) => Ok(cstr.into_raw()),
            Err(_) => Err(AddrInfoError::NullPointer),
        }
    }

    fn null_canonname() -> Self::CanonName {
        ptr::null_mut()
    }

    fn get_family_socktype_protocol(&self) -> (i32, i32, i32) {
        (self.ai_family, self.ai_socktype, self.ai_protocol)
    }

    // Field accessor methods for Drop implementation
    unsafe fn ai_next(&self) -> *mut Self {
        self.ai_next
    }

    unsafe fn ai_addr(&self) -> *mut SOCKADDR {
        self.ai_addr
    }

    unsafe fn ai_family(&self) -> i32 {
        self.ai_family
    }

    unsafe fn ai_canonname(&self) -> Self::CanonName {
        self.ai_canonname
    }

    unsafe fn free_canonname(canonname: Self::CanonName) {
        if !canonname.is_null() {
            unsafe {
                let ptr = canonname;
                let len = (0..).take_while(|&i| *ptr.offset(i) != 0).count();
                let layout = Layout::array::<u16>(len + 1).unwrap();
                std::alloc::dealloc(canonname as *mut u8, layout);
            }
        }
    }
}

impl WindowsAddrInfo for ADDRINFOW {
    type CanonName = *mut u16;

    unsafe fn alloc() -> Result<*mut Self, AddrInfoError> {
        unsafe_alloc!(ADDRINFOW, AddrInfoError::AllocationFailed)
    }

    unsafe fn fill(
        ptr: *mut Self,
        flags: i32,
        family: i32,
        socktype: i32,
        protocol: i32,
        addrlen: usize,
        canonname: Self::CanonName,
        addr: *mut SOCKADDR,
        next: *mut Self,
    ) {
        unsafe {
            (*ptr).ai_flags = flags;
            (*ptr).ai_family = family;
            (*ptr).ai_socktype = socktype;
            (*ptr).ai_protocol = protocol;
            (*ptr).ai_addrlen = addrlen;
            (*ptr).ai_canonname = canonname;
            (*ptr).ai_addr = addr;
            (*ptr).ai_next = next;
        }
    }

    unsafe fn set_next(ptr: *mut Self, next: *mut Self) {
        unsafe {
            (*ptr).ai_next = next;
        }
    }

    fn string_to_canonname(s: String) -> Result<Self::CanonName, AddrInfoError> {
        let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let layout = Layout::array::<u16>(wide.len()).map_err(AddrInfoError::LayoutError)?;
        let ptr = unsafe { std::alloc::alloc(layout) as *mut u16 };
        if ptr.is_null() {
            return Err(AddrInfoError::NullPointer);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        }
        Ok(ptr)
    }

    fn null_canonname() -> Self::CanonName {
        ptr::null_mut()
    }

    fn get_family_socktype_protocol(&self) -> (i32, i32, i32) {
        (self.ai_family, self.ai_socktype, self.ai_protocol)
    }

    // Field accessor methods for Drop implementation
    unsafe fn ai_next(&self) -> *mut Self {
        self.ai_next
    }

    unsafe fn ai_addr(&self) -> *mut SOCKADDR {
        self.ai_addr
    }

    unsafe fn ai_family(&self) -> i32 {
        self.ai_family
    }

    unsafe fn ai_canonname(&self) -> Self::CanonName {
        self.ai_canonname
    }

    unsafe fn free_canonname(canonname: Self::CanonName) {
        if !canonname.is_null() {
            unsafe {
                let ptr = canonname;
                let len = (0..).take_while(|&i| *ptr.offset(i) != 0).count();
                let layout = Layout::array::<u16>(len + 1).unwrap();
                std::alloc::dealloc(canonname as *mut u8, layout);
            }
        }
    }
}

/// RAII wrapper for Windows ADDRINFO structures that automatically cleans up on drop
#[derive(Debug)]
pub struct ManagedAddrInfo<T: WindowsAddrInfo> {
    ptr: *mut T,
}

// SAFETY: We ensure thread safety by only accessing the pointer through controlled operations
// and the pointer is only used for memory management within the same process
unsafe impl<T: WindowsAddrInfo> Send for ManagedAddrInfo<T> {}
unsafe impl<T: WindowsAddrInfo> Sync for ManagedAddrInfo<T> {}

impl<T: WindowsAddrInfo> PartialEq for ManagedAddrInfo<T> {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}

impl<T: WindowsAddrInfo> std::hash::Hash for ManagedAddrInfo<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ptr.hash(state);
    }
}

/// Enum to hold either type of ManagedAddrInfo for storage in a single collection
#[derive(PartialEq, Hash)]
pub enum ManagedAddrInfoAny {
    A(ManagedAddrInfo<ADDRINFOA>),
    W(ManagedAddrInfo<ADDRINFOW>),
}

impl Eq for ManagedAddrInfoAny {}

impl std::fmt::Debug for ManagedAddrInfoAny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagedAddrInfoAny::A(info) => write!(f, "A({:p})", info.ptr),
            ManagedAddrInfoAny::W(info) => write!(f, "W({:p})", info.ptr),
        }
    }
}

impl<T: WindowsAddrInfo> ManagedAddrInfo<T> {
    /// Create a new managed ADDRINFO from a raw pointer
    ///
    /// # Safety
    /// The caller must ensure the pointer is valid and was allocated by our system
    pub unsafe fn new(ptr: *mut T) -> Self {
        Self { ptr }
    }

    /// Get the raw pointer (for returning to Windows API)
    pub fn as_ptr(&self) -> *mut T {
        self.ptr
    }
}

impl<T: WindowsAddrInfo> TryFrom<GetAddrInfoResponse> for ManagedAddrInfo<T> {
    type Error = HookError;

    fn try_from(response: GetAddrInfoResponse) -> HookResult<Self> {
        // Check if the response was successful and ensure we have addresses to process
        let dns_lookup = (*response).clone().and_then(|lookup| {
            if lookup.is_empty() {
                Err(ResponseError::DnsLookup(DnsLookupError {
                    kind: ResolveErrorKindInternal::NoRecordsFound(0),
                }))
            } else {
                Ok(lookup)
            }
        })?;

        let mut first_addrinfo: *mut T = ptr::null_mut();
        let mut current_addrinfo: *mut T = ptr::null_mut();

        for lookup_record in dns_lookup.into_iter() {
            // Allocate ADDRINFO structure
            let addrinfo_ptr = unsafe { T::alloc()? };

            // Parse the IP address and create sockaddr
            let (sockaddr_ptr, sockaddr_len, family) = match lookup_record.ip {
                std::net::IpAddr::V4(ipv4) => {
                    let sockaddr_in_ptr =
                        unsafe_alloc!(SOCKADDR_IN, AddrInfoError::AllocationFailed)?;

                    unsafe {
                        (*sockaddr_in_ptr).sin_family = AF_INET as u16;
                        // Port not available in LookupRecord
                        (*sockaddr_in_ptr).sin_port = 0;
                        *(*sockaddr_in_ptr).sin_addr.S_un.S_addr_mut() = u32::from(ipv4).to_be();
                        ptr::write_bytes((*sockaddr_in_ptr).sin_zero.as_mut_ptr(), 0, 8);
                    }

                    (
                        sockaddr_in_ptr as *mut SOCKADDR,
                        mem::size_of::<SOCKADDR_IN>() as INT,
                        AF_INET,
                    )
                }
                std::net::IpAddr::V6(ipv6) => {
                    let sockaddr_in6_ptr =
                        unsafe_alloc!(SOCKADDR_IN6, AddrInfoError::AllocationFailed)?;

                    unsafe {
                        (*sockaddr_in6_ptr).sin6_family = AF_INET6 as u16;
                        // Port not available in LookupRecord
                        (*sockaddr_in6_ptr).sin6_port = 0;
                        (*sockaddr_in6_ptr).sin6_flowinfo = 0;
                        *(*sockaddr_in6_ptr).sin6_addr.u.Byte_mut() = ipv6.octets();
                        // Note: sin6_scope_id field may not be available in this Windows API
                        // version
                    }

                    (
                        sockaddr_in6_ptr as *mut SOCKADDR,
                        mem::size_of::<SOCKADDR_IN6>() as INT,
                        AF_INET6,
                    )
                }
            };

            // Create canonical name if available
            let canonname = if !lookup_record.name.is_empty() {
                T::string_to_canonname(lookup_record.name)?
            } else {
                T::null_canonname()
            };

            // Fill in the ADDRINFO structure
            unsafe {
                T::fill(
                    addrinfo_ptr,
                    0,                     // ai_flags
                    family,                // ai_family
                    SOCK_STREAM,           // ai_socktype (default to STREAM)
                    0,                     // ai_protocol
                    sockaddr_len as usize, // ai_addrlen
                    canonname,             // ai_canonname
                    sockaddr_ptr,          // ai_addr
                    ptr::null_mut(),       // ai_next (will be set in linking)
                );
            }

            // Link into the list
            if first_addrinfo.is_null() {
                first_addrinfo = addrinfo_ptr;
                current_addrinfo = addrinfo_ptr;
            } else {
                unsafe {
                    T::set_next(current_addrinfo, addrinfo_ptr);
                    current_addrinfo = addrinfo_ptr;
                }
            }

            // Note: We can't insert into MANAGED_ADDRINFO here because we don't have
            // the ManagedAddrInfo wrapper. This tracking is done in the calling functions.
            // guard.insert(addrinfo_ptr as usize, ...);
        }

        Ok(unsafe { Self::new(first_addrinfo) })
    }
}

impl<T: WindowsAddrInfo> Drop for ManagedAddrInfo<T> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                // Walk the entire chain and free each node
                let mut current = self.ptr;
                while !current.is_null() {
                    let next = (*current).ai_next();

                    // Free the sockaddr structure
                    let addr = (*current).ai_addr();
                    if !addr.is_null() {
                        let sockaddr_layout = if (*current).ai_family() == AF_INET {
                            Layout::new::<SOCKADDR_IN>()
                        } else {
                            Layout::new::<SOCKADDR_IN6>()
                        };
                        std::alloc::dealloc(addr as *mut u8, sockaddr_layout);
                    }

                    // Free the canonical name
                    let canonname = (*current).ai_canonname();
                    T::free_canonname(canonname);

                    // Free the ADDRINFO structure itself
                    let addrinfo_layout = Layout::new::<T>();
                    std::alloc::dealloc(current as *mut u8, addrinfo_layout);

                    current = next;
                }
            }
        }
    }
}

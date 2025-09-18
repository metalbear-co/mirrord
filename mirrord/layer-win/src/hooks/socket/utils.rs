//! Utility functions for Windows socket operations

use std::{alloc::Layout, mem, net::SocketAddr, ptr};

use mirrord_layer_lib::error::{AddrInfoError, HookError, HookResult};
use mirrord_protocol::error::{DnsLookupError, ResolveErrorKindInternal, ResponseError};

/// Macro to safely allocate memory for Windows structures with error handling
#[macro_export]
macro_rules! unsafe_alloc {
    ($type:ty, $err: expr) => {{
        let layout = std::alloc::Layout::new::<$type>();
        let ptr = unsafe { std::alloc::alloc(layout) as *mut $type };
        if ptr.is_null() { Err($err) } else { Ok(ptr) }
    }};
}

use mirrord_protocol::{dns::GetAddrInfoResponse, outgoing::SocketAddress};
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{ADDRINFOA, ADDRINFOW, AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN},
        ws2ipdef::SOCKADDR_IN6,
    },
    um::winsock2::{HOSTENT, SOCK_STREAM},
};

/// Windows-specific extensions for SocketAddr trait, similar to SocketAddrExtUnix
pub trait SocketAddrExtWin {
    /// Converts a raw Windows SOCKADDR pointer into a more _Rusty_ type
    fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<Self>
    where
        Self: Sized;

    /// Convert to Windows SOCKADDR and write directly to output buffers with validation
    unsafe fn to_windows_sockaddr_checked(
        &self,
        name: *mut SOCKADDR,
        namelen: *mut INT,
    ) -> Result<(), String>;
}

impl SocketAddrExtWin for SocketAddr {
    fn try_from_raw(raw_address: *const SOCKADDR, address_length: INT) -> Option<SocketAddr> {
        unsafe { sockaddr_to_socket_addr(raw_address, address_length) }
    }

    unsafe fn to_windows_sockaddr_checked(
        &self,
        name: *mut SOCKADDR,
        namelen: *mut INT,
    ) -> Result<(), String> {
        // Validate input parameters
        if name.is_null() {
            return Err("name pointer is null".to_string());
        }
        if namelen.is_null() {
            return Err("namelen pointer is null".to_string());
        }

        // Use the socketaddr_to_windows_sockaddr function but convert error
        match unsafe { socketaddr_to_windows_sockaddr(self, name, namelen) } {
            Ok(()) => Ok(()),
            Err(error_code) => Err(format!(
                "Windows sockaddr conversion failed with error code: {}",
                error_code
            )),
        }
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

/// Windows-specific extension trait for SocketAddress conversion
pub trait SocketAddressExtWin {
    /// Convert to Windows SOCKADDR structure
    unsafe fn to_sockaddr(&self) -> Result<(SOCKADDR, INT), String>;

    /// Convert to Windows SOCKADDR and write directly to output buffers with validation
    unsafe fn to_sockaddr_checked(
        &self,
        name: *mut SOCKADDR,
        namelen: *mut INT,
    ) -> Result<(), String>;
}

impl SocketAddressExtWin for SocketAddress {
    unsafe fn to_sockaddr(&self) -> Result<(SOCKADDR, INT), String> {
        unsafe { socket_address_to_sockaddr(self) }
    }

    unsafe fn to_sockaddr_checked(
        &self,
        name: *mut SOCKADDR,
        namelen: *mut INT,
    ) -> Result<(), String> {
        // Validate input parameters
        if name.is_null() {
            return Err("name pointer is null".to_string());
        }
        if namelen.is_null() {
            return Err("namelen pointer is null".to_string());
        }

        // Convert the address
        let (sockaddr, size) = unsafe { self.to_sockaddr()? };

        // Check buffer size
        if unsafe { *namelen } < size {
            return Err(format!(
                "Buffer too small: need {} bytes, have {}",
                size,
                unsafe { *namelen }
            ));
        }

        // Copy data to output buffer
        unsafe {
            std::ptr::copy_nonoverlapping(
                &sockaddr as *const _ as *const u8,
                name as *mut u8,
                size as usize,
            );
            *namelen = size;
        }

        Ok(())
    }
}

/// Convert SocketAddr to Windows SOCKADDR for address return functions
pub unsafe fn socketaddr_to_windows_sockaddr(
    addr: &SocketAddr,
    name: *mut SOCKADDR,
    namelen: *mut INT,
) -> Result<(), i32> {
    if name.is_null() || namelen.is_null() {
        // WSAEFAULT
        return Err(10014);
    }

    // Additional safety check for namelen dereference
    let name_len_value = unsafe { *namelen };
    if name_len_value < 0 {
        // WSAEFAULT - invalid length
        return Err(10014);
    }

    match addr {
        SocketAddr::V4(addr_v4) => {
            let size = std::mem::size_of::<SOCKADDR_IN>() as INT;
            if name_len_value < size {
                // WSAEFAULT - buffer too small
                return Err(10014);
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
        SocketAddr::V6(addr_v6) => {
            let size = std::mem::size_of::<SOCKADDR_IN6>() as INT;
            if name_len_value < size {
                // WSAEFAULT - buffer too small
                return Err(10014);
            }

            let mut sockaddr_in6: SOCKADDR_IN6 = unsafe { std::mem::zeroed() };
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

/// Convert a mirrord SocketAddress to a Windows SOCKADDR for API calls
pub unsafe fn socket_address_to_sockaddr(
    layer_addr: &SocketAddress,
) -> Result<(SOCKADDR, INT), String> {
    let std_addr = match std::net::SocketAddr::try_from(layer_addr.clone()) {
        Ok(addr) => addr,
        Err(e) => {
            return Err(format!("Failed to convert layer address: {:?}", e));
        }
    };

    match std_addr {
        std::net::SocketAddr::V4(addr_v4) => {
            let mut sockaddr_in: SOCKADDR_IN = unsafe { std::mem::zeroed() };
            sockaddr_in.sin_family = AF_INET as u16;
            sockaddr_in.sin_port = addr_v4.port().to_be();
            unsafe {
                *sockaddr_in.sin_addr.S_un.S_addr_mut() = u32::from(*addr_v4.ip()).to_be();
            }
            let sockaddr = unsafe { std::mem::transmute::<SOCKADDR_IN, SOCKADDR>(sockaddr_in) };
            Ok((sockaddr, std::mem::size_of::<SOCKADDR_IN>() as INT))
        }
        std::net::SocketAddr::V6(_) => {
            Err("IPv6 not yet implemented for layer address".to_string())
        }
    }
}

/// Helper function to extract IP address from HOSTENT structure
///
/// SAFETY: This function assumes the HOSTENT pointer is valid and properly formatted.
/// The caller must ensure the pointer is valid and the structure is properly initialized.
pub unsafe fn extract_ip_from_hostent(hostent: *mut HOSTENT) -> Option<String> {
    if hostent.is_null() {
        return None;
    }

    let host = unsafe { &*hostent };

    // Check if we have any addresses
    if host.h_addr_list.is_null() {
        return None;
    }

    // Get the first address pointer
    let first_addr_ptr = unsafe { *host.h_addr_list };
    if first_addr_ptr.is_null() {
        return None;
    }

    // SAFETY: Validate address family and length before accessing memory
    if host.h_length <= 0 || host.h_length > 16 {
        tracing::warn!(
            "extract_ip_from_hostent: invalid address length {}",
            host.h_length
        );
        return None;
    }

    // SAFETY: Validate pointer alignment and basic sanity checks
    if !(first_addr_ptr as usize).is_multiple_of(std::mem::align_of::<u8>()) {
        tracing::warn!("extract_ip_from_hostent: misaligned address pointer");
        return None;
    }

    // Additional safety: verify the pointer is within reasonable bounds
    // This is a basic check - in production, consider using VirtualQuery
    if (first_addr_ptr as usize) < 0x1000 || (first_addr_ptr as usize) > 0x7FFFFFFFFFFF {
        tracing::warn!(
            "extract_ip_from_hostent: suspicious pointer address: {:p}",
            first_addr_ptr
        );
        return None;
    }

    // Extract IP based on address family
    match host.h_addrtype {
        2 => {
            // AF_INET (IPv4)
            if host.h_length == 4 {
                let ip_bytes =
                    unsafe { std::slice::from_raw_parts(first_addr_ptr as *const u8, 4) };
                Some(format!(
                    "{}.{}.{}.{}",
                    ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]
                ))
            } else {
                tracing::warn!(
                    "extract_ip_from_hostent: IPv4 address has invalid length {}",
                    host.h_length
                );
                None
            }
        }
        23 => {
            // AF_INET6 (IPv6)
            if host.h_length == 16 {
                let ip_bytes =
                    unsafe { std::slice::from_raw_parts(first_addr_ptr as *const u8, 16) };
                // Convert to proper IPv6 string format with colon notation
                let ip: Vec<String> = ip_bytes
                    .windows(2)
                    .map(|x| format!("{:02x}{:02x}", x[0], x[1]))
                    .collect();
                let ip = ip.join(":");

                Some(ip)
            } else {
                tracing::warn!(
                    "extract_ip_from_hostent: IPv6 address has invalid length {}",
                    host.h_length
                );
                None
            }
        }
        _ => {
            tracing::debug!(
                "extract_ip_from_hostent: unsupported address family {}",
                host.h_addrtype
            );
            None
        }
    }
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
    if buffer_size <= 0 {
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
                let _ = std::ffi::CString::from_raw(canonname);
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
        // Convert UTF-8 string to UTF-16 wide string
        let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let layout =
            std::alloc::Layout::array::<u16>(wide.len()).map_err(AddrInfoError::LayoutError)?;
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
                // For wide strings, we need to find the length first
                let mut len = 0;
                let mut current = canonname;
                while *current != 0 {
                    len += 1;
                    current = current.add(1);
                }
                if len > 0 {
                    let layout = std::alloc::Layout::array::<u16>(len + 1).unwrap();
                    std::alloc::dealloc(canonname as *mut u8, layout);
                }
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
                        std::mem::size_of::<SOCKADDR_IN>() as INT,
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
                        std::mem::size_of::<SOCKADDR_IN6>() as INT,
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

//! Utility functions for Windows socket operations
use std::{alloc::Layout, convert::TryFrom, mem, ptr};

use mirrord_protocol::{
    dns::GetAddrInfoResponse,
    error::{DnsLookupError, ResolveErrorKindInternal, ResponseError},
};
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{ADDRINFOA, ADDRINFOW, AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN},
        ws2ipdef::SOCKADDR_IN6,
    },
    um::winsock2::SOCK_STREAM,
};

use crate::{
    error::{AddrInfoError, HookError, HookResult},
    unsafe_alloc,
};

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
        let wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
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

    /// Propagate the requested service port into each sockaddr entry
    pub fn apply_port(&mut self, port: u16) {
        unsafe {
            let mut current = self.ptr;
            while !current.is_null() {
                let addr = (*current).ai_addr();
                if !addr.is_null() {
                    match (*current).ai_family() {
                        AF_INET => {
                            let sockaddr_in_ptr = addr as *mut SOCKADDR_IN;
                            (*sockaddr_in_ptr).sin_port = port.to_be();
                        }
                        AF_INET6 => {
                            let sockaddr_in6_ptr = addr as *mut SOCKADDR_IN6;
                            (*sockaddr_in6_ptr).sin6_port = port.to_be();
                        }
                        _ => {}
                    }
                }
                current = (*current).ai_next();
            }
        }
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
                        // Port applied later via ManagedAddrInfo::apply_port
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
                        // Port applied later via ManagedAddrInfo::apply_port
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

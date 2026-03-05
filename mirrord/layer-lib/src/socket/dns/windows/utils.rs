//! Utility functions for Windows socket operations
use std::{convert::TryFrom, mem, net::IpAddr, ptr};

use winapi::{
    shared::{
        in6addr::IN6_ADDR,
        inaddr::IN_ADDR,
        minwindef::INT,
        ws2def::{ADDRINFOA, ADDRINFOW, AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN},
        ws2ipdef::SOCKADDR_IN6,
    },
    um::winsock2::SOCK_STREAM,
};

use crate::error::{HookError, HookResult};

const IPV4_ADDR_LEN: usize = mem::size_of::<IN_ADDR>();
const IPV6_ADDR_LEN: usize = mem::size_of::<IN6_ADDR>();

/// Owned IP bytes shared across HOSTENT and ADDRINFO construction.
#[derive(Debug, Clone, Copy)]
pub enum IpAddrBytes {
    V4([u8; IPV4_ADDR_LEN]),
    V6([u8; IPV6_ADDR_LEN]),
}

impl IpAddrBytes {
    pub fn family(&self) -> i32 {
        match self {
            IpAddrBytes::V4(_) => AF_INET,
            IpAddrBytes::V6(_) => AF_INET6,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            IpAddrBytes::V4(_) => IPV4_ADDR_LEN,
            IpAddrBytes::V6(_) => IPV6_ADDR_LEN,
        }
    }

    pub fn as_ptr(&self) -> *const u8 {
        match self {
            IpAddrBytes::V4(bytes) | IpAddrBytes::V6(bytes) => bytes.as_ptr(),
        }
    }

    pub fn to_sockaddr_box(&self) -> AddrStorage {
        match self {
            IpAddrBytes::V4(bytes) => {
                let mut addr: SOCKADDR_IN = unsafe { mem::zeroed() };
                let ipv4 = std::net::Ipv4Addr::from(*bytes);
                addr.sin_family = AF_INET as u16;
                addr.sin_port = 0;
                unsafe {
                    *addr.sin_addr.S_un.S_addr_mut() = u32::from(ipv4).to_be();
                    ptr::write_bytes(addr.sin_zero.as_mut_ptr(), 0, 8);
                }
                AddrStorage::V4(Box::new(addr))
            }
            IpAddrBytes::V6(bytes) => {
                let mut addr: SOCKADDR_IN6 = unsafe { mem::zeroed() };
                addr.sin6_family = AF_INET6 as u16;
                addr.sin6_port = 0;
                addr.sin6_flowinfo = 0;
                unsafe {
                    *addr.sin6_addr.u.Byte_mut() = *bytes;
                }
                AddrStorage::V6(Box::new(addr))
            }
        }
    }
}

impl From<IpAddr> for IpAddrBytes {
    fn from(value: IpAddr) -> Self {
        match value {
            IpAddr::V4(ipv4) => IpAddrBytes::V4(ipv4.octets()),
            IpAddr::V6(ipv6) => IpAddrBytes::V6(ipv6.octets()),
        }
    }
}

/// Owned sockaddr storage for ADDRINFO chains.
#[derive(Debug)]
pub enum AddrStorage {
    V4(Box<SOCKADDR_IN>),
    V6(Box<SOCKADDR_IN6>),
}

impl AddrStorage {
    pub fn family(&self) -> i32 {
        match self {
            AddrStorage::V4(_) => AF_INET,
            AddrStorage::V6(_) => AF_INET6,
        }
    }

    pub fn addrlen(&self) -> usize {
        match self {
            AddrStorage::V4(_) => mem::size_of::<SOCKADDR_IN>(),
            AddrStorage::V6(_) => mem::size_of::<SOCKADDR_IN6>(),
        }
    }

    pub fn as_ptr(&mut self) -> *mut SOCKADDR {
        match self {
            AddrStorage::V4(addr) => addr.as_mut() as *mut SOCKADDR_IN as *mut SOCKADDR,
            AddrStorage::V6(addr) => addr.as_mut() as *mut SOCKADDR_IN6 as *mut SOCKADDR,
        }
    }
}

/// Trait to abstract over Windows ADDRINFO types (ADDRINFOA and ADDRINFOW).
/// Prefer interacting with these pointers through [`ManagedAddrInfo`], which owns the storage
/// for nodes, sockaddrs, and canonical names.
pub trait WindowsAddrInfo: Sized {
    type CanonName;
    type CanonNameOwned;

    /// Initialize default fields (`ai_flags`, `ai_socktype`, `ai_protocol`, and `ai_next`).
    fn init_defaults(node: &mut Self);

    /// Fill the structure with the given parameters.
    fn fill(
        node: &mut Self,
        family: i32,
        addrlen: usize,
        canonname: Self::CanonName,
        addr: *mut SOCKADDR,
    );

    /// Set the next pointer.
    fn set_next(node: &mut Self, next: *mut Self);

    /// Convert a string to the appropriate owned canonical name type.
    fn canonname_from_string(s: String) -> HookResult<Self::CanonNameOwned>;

    /// Get a raw pointer for a canonical name.
    fn canonname_ptr(owned: &mut Self::CanonNameOwned) -> Self::CanonName;

    /// Get null canonical name pointer.
    fn null_canonname_ptr() -> Self::CanonName;

    /// Extract family, socktype, and protocol from this structure (for hints processing)
    fn get_family_socktype_protocol(&self) -> (i32, i32, i32);

    // Raw field accessors used by ManagedAddrInfo traversal.
    /// Get the ai_next field.
    fn ai_next(&self) -> *mut Self;

    /// Get the ai_addr field.
    fn ai_addr(&self) -> *mut SOCKADDR;

    /// Get the ai_family field.
    fn ai_family(&self) -> i32;
}

impl WindowsAddrInfo for ADDRINFOA {
    type CanonName = *mut i8;
    type CanonNameOwned = std::ffi::CString;

    fn init_defaults(node: &mut Self) {
        node.ai_flags = 0;
        node.ai_socktype = SOCK_STREAM;
        node.ai_protocol = 0;
        node.ai_next = ptr::null_mut();
    }

    fn fill(
        node: &mut Self,
        family: i32,
        addrlen: usize,
        canonname: Self::CanonName,
        addr: *mut SOCKADDR,
    ) {
        node.ai_family = family;
        node.ai_addrlen = addrlen;
        node.ai_canonname = canonname;
        node.ai_addr = addr;
    }

    fn set_next(node: &mut Self, next: *mut Self) {
        node.ai_next = next;
    }

    fn canonname_from_string(s: String) -> HookResult<Self::CanonNameOwned> {
        Ok(std::ffi::CString::new(s)?)
    }

    fn canonname_ptr(owned: &mut Self::CanonNameOwned) -> Self::CanonName {
        owned.as_ptr() as *mut i8
    }

    fn null_canonname_ptr() -> Self::CanonName {
        ptr::null_mut()
    }

    fn get_family_socktype_protocol(&self) -> (i32, i32, i32) {
        (self.ai_family, self.ai_socktype, self.ai_protocol)
    }

    // Raw field accessors used by ManagedAddrInfo traversal.
    fn ai_next(&self) -> *mut Self {
        self.ai_next
    }

    fn ai_addr(&self) -> *mut SOCKADDR {
        self.ai_addr
    }

    fn ai_family(&self) -> i32 {
        self.ai_family
    }
}

impl WindowsAddrInfo for ADDRINFOW {
    type CanonName = *mut u16;
    type CanonNameOwned = Vec<u16>;

    fn init_defaults(node: &mut Self) {
        node.ai_flags = 0;
        node.ai_socktype = SOCK_STREAM;
        node.ai_protocol = 0;
        node.ai_next = ptr::null_mut();
    }

    fn fill(
        node: &mut Self,
        family: i32,
        addrlen: usize,
        canonname: Self::CanonName,
        addr: *mut SOCKADDR,
    ) {
        node.ai_family = family;
        node.ai_addrlen = addrlen;
        node.ai_canonname = canonname;
        node.ai_addr = addr;
    }

    fn set_next(node: &mut Self, next: *mut Self) {
        node.ai_next = next;
    }

    fn canonname_from_string(s: String) -> HookResult<Self::CanonNameOwned> {
        let wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        Ok(wide)
    }

    fn canonname_ptr(owned: &mut Self::CanonNameOwned) -> Self::CanonName {
        owned.as_mut_ptr()
    }

    fn null_canonname_ptr() -> Self::CanonName {
        ptr::null_mut()
    }

    fn get_family_socktype_protocol(&self) -> (i32, i32, i32) {
        (self.ai_family, self.ai_socktype, self.ai_protocol)
    }

    // Raw field accessors used by ManagedAddrInfo traversal.
    fn ai_next(&self) -> *mut Self {
        self.ai_next
    }

    fn ai_addr(&self) -> *mut SOCKADDR {
        self.ai_addr
    }

    fn ai_family(&self) -> i32 {
        self.ai_family
    }
}

/// RAII wrapper for Windows ADDRINFO structures that automatically cleans up on drop
#[derive(Debug)]
pub struct ManagedAddrInfo<T: WindowsAddrInfo> {
    nodes: Vec<Box<T>>,
    sockaddrs: Vec<AddrStorage>,
    _canonnames: Vec<Option<T::CanonNameOwned>>,
}

// SAFETY: We ensure thread safety by only accessing the pointer through controlled operations
// and the pointer is only used for memory management within the same process
unsafe impl<T: WindowsAddrInfo> Send for ManagedAddrInfo<T> {}
unsafe impl<T: WindowsAddrInfo> Sync for ManagedAddrInfo<T> {}

impl<T: WindowsAddrInfo> PartialEq for ManagedAddrInfo<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_ptr() == other.as_ptr()
    }
}

impl<T: WindowsAddrInfo> std::hash::Hash for ManagedAddrInfo<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_ptr().hash(state);
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
            ManagedAddrInfoAny::A(info) => write!(f, "A({:p})", info.as_ptr()),
            ManagedAddrInfoAny::W(info) => write!(f, "W({:p})", info.as_ptr()),
        }
    }
}

impl<T: WindowsAddrInfo> ManagedAddrInfo<T> {
    /// Get the raw pointer (for returning to Windows API)
    pub fn as_ptr(&self) -> *mut T {
        self.nodes
            .first()
            .map(|node| node.as_ref() as *const T as *mut T)
            .unwrap_or(ptr::null_mut())
    }

    /// Propagate the requested service port into each sockaddr entry
    pub fn apply_port(&mut self, port: u16) {
        for addr in &mut self.sockaddrs {
            match addr {
                AddrStorage::V4(sockaddr) => {
                    sockaddr.sin_port = port.to_be();
                }
                AddrStorage::V6(sockaddr) => {
                    sockaddr.sin6_port = port.to_be();
                }
            }
        }
    }
}

impl<T: WindowsAddrInfo> TryFrom<Vec<(String, IpAddr)>> for ManagedAddrInfo<T> {
    type Error = HookError;

    fn try_from(records: Vec<(String, IpAddr)>) -> HookResult<Self> {
        if records.is_empty() {
            return Err(HookError::DNSNoName);
        }

        let mut nodes: Vec<Box<T>> = Vec::with_capacity(records.len());
        let mut sockaddrs: Vec<AddrStorage> = Vec::with_capacity(records.len());
        let mut canonnames: Vec<Option<T::CanonNameOwned>> = Vec::with_capacity(records.len());

        for (name, ip) in records.into_iter() {
            let ip_bytes = IpAddrBytes::from(ip);
            let mut sockaddr_storage = ip_bytes.to_sockaddr_box();

            let sockaddr_ptr = sockaddr_storage.as_ptr();
            let sockaddr_len = sockaddr_storage.addrlen() as INT;
            let family = sockaddr_storage.family();

            // Create canonical name if available
            let mut canonname_owned = if !name.is_empty() {
                Some(T::canonname_from_string(name)?)
            } else {
                None
            };
            let canonname_ptr = match canonname_owned.as_mut() {
                Some(owned) => T::canonname_ptr(owned),
                None => T::null_canonname_ptr(),
            };

            // SAFETY: T is a C struct; we immediately initialize the fields we rely on via
            // `init_defaults` and `fill`.
            let mut node: Box<T> = Box::new(unsafe { mem::MaybeUninit::zeroed().assume_init() });
            T::init_defaults(&mut node);

            // Fill in the ADDRINFO structure
            T::fill(
                node.as_mut(),
                family,
                sockaddr_len as usize,
                canonname_ptr,
                sockaddr_ptr,
            );

            sockaddrs.push(sockaddr_storage);
            canonnames.push(canonname_owned);
            nodes.push(node);
        }

        for i in 0..nodes.len().saturating_sub(1) {
            let next_ptr = nodes[i + 1].as_mut() as *mut T;
            T::set_next(nodes[i].as_mut(), next_ptr);
        }

        Ok(Self {
            nodes,
            sockaddrs: sockaddrs,
            _canonnames: canonnames,
        })
    }
}

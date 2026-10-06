//! Resolves macOS DNS-SD (`<dns_sd.h>`) address queries through the agent.
//!
//! On macOS, Bun talks to `mDNSResponder` directly through the DNS-SD API. Without these hooks,
//! those lookups are answered by the local machine, so cluster names fail to resolve.
//!
//! DNS-SD is asynchronous. Starting a query gives the caller a `DNSServiceRef`. The caller then
//! watches the socket behind that ref (or behind the shared connection it was started on) in its
//! own event loop, and calls `DNSServiceProcessResult` once the socket is readable. That call is
//! what runs the query's callback. The socket belongs to `mDNSResponder`, so we have no way of
//! making it readable when our answer is ready.
//!
//! Instead, we get the daemon to wake the caller up for us:
//!
//! 1. When a query comes in for a name we resolve remotely, we still start a real query on the
//!    caller's ref or connection, but for [`DECOY_HOSTNAME`], with our own callback and a
//!    [`RemoteQuery`] as its context. The daemon answers that right away, so the caller's socket
//!    becomes readable.
//! 2. The caller calls `DNSServiceProcessResult`, which runs our callback with the decoy's reply.
//!    We ignore that reply. Instead, we ask the agent to resolve the real name, and call the
//!    caller's callback with the answer, formatted like a reply from the daemon. If more decoy
//!    replies come in later, we ignore them too.
//! 3. When the caller deallocates the query (or the connection it was started on), we free its
//!    [`RemoteQuery`].
//!
//! In Step 2, the agent lookup round trip blocks the call's thread - Bun's event loop :(
//! This is a tradeoff we make for simplicity.
//!
//! We rely on the DNS-SD rule that a ref is only used from one thread at a time. That means a
//! query's callbacks and its deallocation never run concurrently. The only way a query can be
//! deallocated while we're delivering its answer is from inside the caller's own callback.

use std::{
    cell::Cell,
    collections::HashMap,
    ffi::{CStr, CString},
    net::IpAddr,
    sync::{Arc, LazyLock, OnceLock, Weak},
};

use libc::{c_char, c_void, sockaddr, sockaddr_in, sockaddr_in6};
use mirrord_layer_lib::{
    detour::{Detour, DetourGuard},
    error::{HookError, getaddrinfo_error_code},
    mutex::Mutex,
    socket::dns::remote_getaddrinfo,
};
use mirrord_layer_macro::hook_guard_fn;

use crate::{hooks::HookManager, replace};

type DNSServiceRef = *mut c_void;
type DNSServiceErrorType = i32;

type DNSServiceGetAddrInfoReply = unsafe extern "C" fn(
    sd_ref: DNSServiceRef,
    flags: u32,
    interface_index: u32,
    error_code: DNSServiceErrorType,
    hostname: *const c_char,
    address: *const sockaddr,
    ttl: u32,
    context: *mut c_void,
);

type DNSServiceQueryRecordReply = unsafe extern "C" fn(
    sd_ref: DNSServiceRef,
    flags: u32,
    interface_index: u32,
    error_code: DNSServiceErrorType,
    fullname: *const c_char,
    rrtype: u16,
    rrclass: u16,
    rdlen: u16,
    rdata: *const c_void,
    ttl: u32,
    context: *mut c_void,
);

// Values from Apple's `dns_sd.h`, see `kDNSServiceErr_*`, `kDNSServiceFlags*`,
// `kDNSServiceProtocol_*`, `kDNSServiceType_*` and `kDNSServiceClass_*` in
// <https://github.com/apple-oss-distributions/mDNSResponder/blob/main/mDNSShared/dns_sd.h>.
// The same header ships with the macOS SDK, at `$(xcrun --show-sdk-path)/usr/include/dns_sd.h`.
const ERR_NO_ERROR: DNSServiceErrorType = 0;
const ERR_UNKNOWN: DNSServiceErrorType = -65537;
const ERR_NO_SUCH_RECORD: DNSServiceErrorType = -65554;
const ERR_TIMEOUT: DNSServiceErrorType = -65568;

const FLAGS_MORE_COMING: u32 = 0x1;
const FLAGS_ADD: u32 = 0x2;
const FLAGS_SHARE_CONNECTION: u32 = 0x4000;
/// `ForceMulticast`, `Validate` and `ValidateOptional`: queries the agent can't answer.
const FLAGS_PASS_THROUGH: u32 = 0x400 | 0x20_0000 | 0x80_0000;

const PROTOCOL_IPV4: u32 = 0x1;
const PROTOCOL_IPV6: u32 = 0x2;

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const CLASS_IN: u16 = 1;

/// The agent doesn't tell us the record's TTL.
const TTL: u32 = 30;

/// Answered by `mDNSResponder` itself, right away.
const DECOY_HOSTNAME: &CStr = c"localhost";

/// The kind of query the caller started, and where its answer goes.
enum Query {
    /// `DNSServiceGetAddrInfo` or `DNSServiceGetAddrInfoEx`.
    GetAddrInfo {
        callback: DNSServiceGetAddrInfoReply,
        protocol: u32,
    },
    /// `DNSServiceQueryRecord` or `DNSServiceQueryRecordWithAttribute`.
    QueryRecord {
        callback: DNSServiceQueryRecordReply,
        rrtype: u16,
    },
}

/// A query we took over, the context of its decoy query.
struct RemoteQuery {
    query: Query,
    /// The name as the daemon reports it, fully qualified.
    fullname: CString,
    context: *mut c_void,
    /// The shared connection the query was started on, if any.
    connection: usize,
    resolution: Arc<Resolution>,
    delivered: Cell<bool>,
    delivering: Cell<bool>,
    cancelled: Cell<bool>,
}

/// The agent's answer for a name, shared by the queries of one lookup (Bun asks for A and AAAA
/// with two queries), so each lookup costs a single round trip.
struct Resolution {
    key: ResolutionKey,
    addresses: OnceLock<Result<Vec<IpAddr>, DNSServiceErrorType>>,
}

/// Identifies the lookup a query belongs to: the queries of one lookup ask for the same name, with
/// the same callback and context.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ResolutionKey {
    name: String,
    /// The caller's callback, as an address.
    callback: usize,
    /// The caller's context, as an address.
    context: usize,
}

#[derive(Default)]
struct Registry {
    /// Queries we took over, so `DNSServiceRefDeallocate` can find and free them.
    ///
    /// - Key: the address of the query's `DNSServiceRef`, as the caller got it back from the query
    ///   call (the decoy query's ref).
    /// - Value: the address of the query's [`RemoteQuery`], a leaked [`Box`] that is also the
    ///   decoy query's context. It's freed by [`RemoteQuery::release`] or
    ///   [`RemoteQuery::deliver`].
    ///
    /// Both are stored as `usize` because raw pointers aren't [`Send`], and this lives in a
    /// static.
    queries: HashMap<usize, usize>,
    /// Resolutions not started yet, that new queries of the same lookup join.
    unresolved: HashMap<ResolutionKey, Weak<Resolution>>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Default::default);

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Resolution {
    fn join(key: ResolutionKey) -> Arc<Self> {
        let mut registry = registry();

        if let Some(resolution) = registry.unresolved.get(&key).and_then(Weak::upgrade) {
            return resolution;
        }

        let resolution = Arc::new(Self {
            key: key.clone(),
            addresses: OnceLock::new(),
        });
        registry.unresolved.insert(key, Arc::downgrade(&resolution));
        resolution
    }

    fn addresses(&self) -> &Result<Vec<IpAddr>, DNSServiceErrorType> {
        self.addresses.get_or_init(|| {
            registry().unresolved.remove(&self.key);

            let _guard = DetourGuard::new();
            remote_getaddrinfo(self.key.name.clone(), 0, 0, 0, 0, 0)
                .map(|records| records.into_iter().map(|(_, address)| address).collect())
                .map_err(dns_sd_error)
        })
    }
}

impl Drop for Resolution {
    /// A lookup deallocated before it was resolved leaves its key behind, unless a newer lookup
    /// already took it over.
    fn drop(&mut self) {
        if self.addresses.get().is_some() {
            return;
        }

        let mut registry = registry();
        if registry
            .unresolved
            .get(&self.key)
            .is_some_and(|resolution| resolution.strong_count() == 0)
        {
            registry.unresolved.remove(&self.key);
        }
    }
}

fn dns_sd_error(error: HookError) -> DNSServiceErrorType {
    match getaddrinfo_error_code(error) {
        libc::EAI_NONAME | libc::EAI_NODATA => ERR_NO_SUCH_RECORD,
        libc::EAI_AGAIN => ERR_TIMEOUT,
        _ => ERR_UNKNOWN,
    }
}

impl RemoteQuery {
    /// Returns [`None`] when the query should go to the daemon untouched.
    unsafe fn new(
        sd_ref: *mut DNSServiceRef,
        flags: u32,
        interface_index: u32,
        hostname: *const c_char,
        query: Query,
        context: *mut c_void,
    ) -> Option<Box<Self>> {
        if sd_ref.is_null()
            || hostname.is_null()
            || interface_index != 0
            || flags & FLAGS_PASS_THROUGH != 0
        {
            return None;
        }

        let name = unsafe { CStr::from_ptr(hostname) }.to_str().ok()?;
        // DNS-SD queries carry no port, so like `gethostbyname` (and `getaddrinfo` without a
        // service), only the DNS filters without a port apply.
        if name.parse::<IpAddr>().is_ok()
            || !matches!(
                crate::setup().dns_selector().check_query(name, 0),
                Detour::Success(())
            )
        {
            return None;
        }

        let callback = match query {
            Query::GetAddrInfo { callback, .. } => callback as usize,
            Query::QueryRecord { callback, .. } => callback as usize,
        };
        let fullname = if name.ends_with('.') {
            name.to_owned()
        } else {
            format!("{name}.")
        };
        let connection = if flags & FLAGS_SHARE_CONNECTION != 0 {
            unsafe { *sd_ref as usize }
        } else {
            0
        };

        Some(Box::new(Self {
            query,
            fullname: CString::new(fullname).ok()?,
            context,
            connection,
            resolution: Resolution::join(ResolutionKey {
                name: name.to_owned(),
                callback,
                context: context as usize,
            }),
            delivered: Cell::new(false),
            delivering: Cell::new(false),
            cancelled: Cell::new(false),
        }))
    }

    /// Starts the decoy query with `start_decoy` and keeps track of the result.
    unsafe fn start(
        self: Box<Self>,
        sd_ref: *mut DNSServiceRef,
        start_decoy: impl FnOnce(*mut c_void) -> DNSServiceErrorType,
    ) -> DNSServiceErrorType {
        let remote_query = Box::into_raw(self);
        let result = start_decoy(remote_query.cast());

        if result == ERR_NO_ERROR {
            registry()
                .queries
                .insert(unsafe { *sd_ref } as usize, remote_query as usize);
        } else {
            drop(unsafe { Box::from_raw(remote_query) });
        }

        result
    }

    /// Hands the agent's answer to the caller, on the first decoy reply.
    unsafe fn deliver(remote_query: *mut Self, sd_ref: DNSServiceRef) {
        let this = unsafe { &*remote_query };
        if this.delivered.replace(true) {
            return;
        }

        let replies = this.replies();
        this.delivering.set(true);
        for (index, reply) in replies.iter().enumerate() {
            if this.cancelled.get() {
                break;
            }

            let flags = reply.flags
                | if index + 1 < replies.len() {
                    FLAGS_MORE_COMING
                } else {
                    0
                };
            unsafe { this.call(sd_ref, flags, reply) };
        }
        this.delivering.set(false);

        if this.cancelled.get() {
            drop(unsafe { Box::from_raw(remote_query) });
        }
    }

    /// The caller's query was deallocated.
    unsafe fn release(remote_query: *mut Self) {
        if unsafe { &*remote_query }.delivering.get() {
            unsafe { &*remote_query }.cancelled.set(true);
        } else {
            drop(unsafe { Box::from_raw(remote_query) });
        }
    }

    /// One reply per address, or an error per family (record type) without any.
    fn replies(&self) -> Vec<Reply> {
        let families: &[u16] = match self.query {
            Query::GetAddrInfo { protocol, .. } => match protocol & (PROTOCOL_IPV4 | PROTOCOL_IPV6)
            {
                PROTOCOL_IPV4 => &[TYPE_A],
                PROTOCOL_IPV6 => &[TYPE_AAAA],
                _ => &[TYPE_A, TYPE_AAAA],
            },
            Query::QueryRecord { rrtype, .. } => {
                if rrtype == TYPE_A {
                    &[TYPE_A]
                } else {
                    &[TYPE_AAAA]
                }
            }
        };

        let mut replies = Vec::new();
        for &family in families {
            let found = match self.resolution.addresses() {
                Ok(addresses) => {
                    let before = replies.len();
                    replies.extend(
                        addresses
                            .iter()
                            .filter(|address| address.is_ipv4() == (family == TYPE_A))
                            .map(|&address| Reply {
                                flags: FLAGS_ADD,
                                error: ERR_NO_ERROR,
                                family,
                                address: Some(address),
                            }),
                    );
                    replies.len() > before
                }
                Err(_) => false,
            };

            if !found {
                let error = self
                    .resolution
                    .addresses()
                    .as_ref()
                    .err()
                    .copied()
                    .unwrap_or(ERR_NO_SUCH_RECORD);
                replies.push(Reply {
                    flags: 0,
                    error,
                    family,
                    address: None,
                });
            }
        }

        replies
    }

    unsafe fn call(&self, sd_ref: DNSServiceRef, flags: u32, reply: &Reply) {
        match self.query {
            Query::GetAddrInfo { callback, .. } => {
                let storage = SockaddrStorage::new(reply.family, reply.address);
                unsafe {
                    callback(
                        sd_ref,
                        flags,
                        0,
                        reply.error,
                        self.fullname.as_ptr(),
                        storage.as_ptr(),
                        if reply.address.is_some() { TTL } else { 0 },
                        self.context,
                    )
                }
            }
            Query::QueryRecord { callback, .. } => {
                let (rdata, rdlen) = match reply.address {
                    Some(IpAddr::V4(address)) => (address.octets().to_vec(), 4),
                    Some(IpAddr::V6(address)) => (address.octets().to_vec(), 16),
                    None => (Vec::new(), 0),
                };
                unsafe {
                    callback(
                        sd_ref,
                        flags,
                        0,
                        reply.error,
                        self.fullname.as_ptr(),
                        reply.family,
                        CLASS_IN,
                        rdlen,
                        if rdlen == 0 {
                            std::ptr::null()
                        } else {
                            rdata.as_ptr().cast()
                        },
                        if reply.address.is_some() { TTL } else { 0 },
                        self.context,
                    )
                }
            }
        }
    }
}

/// One call to the caller's callback, built from the agent's answer by [`RemoteQuery::replies`]
/// and made by [`RemoteQuery::call`].
///
/// Each address the agent found becomes one reply. A family with no addresses (or a failed
/// lookup) gets a single reply carrying the error instead, like `mDNSResponder` does.
struct Reply {
    /// [`FLAGS_ADD`] for an address, `0` for an error. [`FLAGS_MORE_COMING`] is added when the
    /// replies are delivered, since it depends on whether this is the last one.
    flags: u32,
    /// [`ERR_NO_ERROR`] for an address, otherwise why this family has none.
    error: DNSServiceErrorType,
    /// [`TYPE_A`] or [`TYPE_AAAA`]. Also decides the `sockaddr` family for `DNSServiceGetAddrInfo`
    /// replies, which carry one even for errors.
    family: u16,
    /// [`None`] for an error.
    address: Option<IpAddr>,
}

/// A `sockaddr` of the reply's family, zeroed for errors, like the daemon's.
enum SockaddrStorage {
    V4(sockaddr_in),
    V6(sockaddr_in6),
}

impl SockaddrStorage {
    fn new(family: u16, address: Option<IpAddr>) -> Self {
        if family == TYPE_A {
            let mut storage: sockaddr_in = unsafe { std::mem::zeroed() };
            storage.sin_len = size_of::<sockaddr_in>() as u8;
            storage.sin_family = libc::AF_INET as u8;
            if let Some(IpAddr::V4(address)) = address {
                storage.sin_addr.s_addr = u32::from(address).to_be();
            }
            Self::V4(storage)
        } else {
            let mut storage: sockaddr_in6 = unsafe { std::mem::zeroed() };
            storage.sin6_len = size_of::<sockaddr_in6>() as u8;
            storage.sin6_family = libc::AF_INET6 as u8;
            if let Some(IpAddr::V6(address)) = address {
                storage.sin6_addr.s6_addr = address.octets();
            }
            Self::V6(storage)
        }
    }

    fn as_ptr(&self) -> *const sockaddr {
        match self {
            Self::V4(storage) => (storage as *const sockaddr_in).cast(),
            Self::V6(storage) => (storage as *const sockaddr_in6).cast(),
        }
    }
}

unsafe extern "C" fn get_addr_info_decoy_reply(
    sd_ref: DNSServiceRef,
    _flags: u32,
    _interface_index: u32,
    _error_code: DNSServiceErrorType,
    _hostname: *const c_char,
    _address: *const sockaddr,
    _ttl: u32,
    context: *mut c_void,
) {
    unsafe { RemoteQuery::deliver(context.cast(), sd_ref) }
}

unsafe extern "C" fn query_record_decoy_reply(
    sd_ref: DNSServiceRef,
    _flags: u32,
    _interface_index: u32,
    _error_code: DNSServiceErrorType,
    _fullname: *const c_char,
    _rrtype: u16,
    _rrclass: u16,
    _rdlen: u16,
    _rdata: *const c_void,
    _ttl: u32,
    context: *mut c_void,
) {
    unsafe { RemoteQuery::deliver(context.cast(), sd_ref) }
}

/// Hook for `DNSServiceGetAddrInfo`: takes over address lookups for names we resolve remotely.
#[allow(non_snake_case)]
#[hook_guard_fn]
unsafe extern "C" fn DNSServiceGetAddrInfo_detour(
    sd_ref: *mut DNSServiceRef,
    flags: u32,
    interface_index: u32,
    protocol: u32,
    hostname: *const c_char,
    callback: Option<DNSServiceGetAddrInfoReply>,
    context: *mut c_void,
) -> DNSServiceErrorType {
    unsafe {
        let remote_query = callback.and_then(|callback| {
            RemoteQuery::new(
                sd_ref,
                flags,
                interface_index,
                hostname,
                Query::GetAddrInfo { callback, protocol },
                context,
            )
        });

        match remote_query {
            Some(remote_query) => remote_query.start(sd_ref, |remote_query| {
                FN_DNSSERVICEGETADDRINFO(
                    sd_ref,
                    flags,
                    0,
                    protocol,
                    DECOY_HOSTNAME.as_ptr(),
                    Some(get_addr_info_decoy_reply),
                    remote_query,
                )
            }),
            None => FN_DNSSERVICEGETADDRINFO(
                sd_ref,
                flags,
                interface_index,
                protocol,
                hostname,
                callback,
                context,
            ),
        }
    }
}

/// Hook for the private `DNSServiceGetAddrInfoEx` (`DNSServiceGetAddrInfo` with a
/// `DNSServiceAttribute`), which Bun 1.4.2 uses.
#[allow(non_snake_case)]
#[hook_guard_fn]
unsafe extern "C" fn DNSServiceGetAddrInfoEx_detour(
    sd_ref: *mut DNSServiceRef,
    flags: u32,
    interface_index: u32,
    protocol: u32,
    hostname: *const c_char,
    attribute: *const c_void,
    callback: Option<DNSServiceGetAddrInfoReply>,
    context: *mut c_void,
) -> DNSServiceErrorType {
    unsafe {
        let remote_query = callback.and_then(|callback| {
            RemoteQuery::new(
                sd_ref,
                flags,
                interface_index,
                hostname,
                Query::GetAddrInfo { callback, protocol },
                context,
            )
        });

        match remote_query {
            Some(remote_query) => remote_query.start(sd_ref, |remote_query| {
                FN_DNSSERVICEGETADDRINFOEX(
                    sd_ref,
                    flags,
                    0,
                    protocol,
                    DECOY_HOSTNAME.as_ptr(),
                    attribute,
                    Some(get_addr_info_decoy_reply),
                    remote_query,
                )
            }),
            None => FN_DNSSERVICEGETADDRINFOEX(
                sd_ref,
                flags,
                interface_index,
                protocol,
                hostname,
                attribute,
                callback,
                context,
            ),
        }
    }
}

/// Hook for `DNSServiceQueryRecord`: takes over A and AAAA queries for names we resolve remotely.
#[allow(non_snake_case)]
#[hook_guard_fn]
unsafe extern "C" fn DNSServiceQueryRecord_detour(
    sd_ref: *mut DNSServiceRef,
    flags: u32,
    interface_index: u32,
    fullname: *const c_char,
    rrtype: u16,
    rrclass: u16,
    callback: Option<DNSServiceQueryRecordReply>,
    context: *mut c_void,
) -> DNSServiceErrorType {
    unsafe {
        let remote_query = callback
            .filter(|_| rrclass == CLASS_IN && matches!(rrtype, TYPE_A | TYPE_AAAA))
            .and_then(|callback| {
                RemoteQuery::new(
                    sd_ref,
                    flags,
                    interface_index,
                    fullname,
                    Query::QueryRecord { callback, rrtype },
                    context,
                )
            });

        match remote_query {
            Some(remote_query) => remote_query.start(sd_ref, |remote_query| {
                FN_DNSSERVICEQUERYRECORD(
                    sd_ref,
                    flags,
                    0,
                    DECOY_HOSTNAME.as_ptr(),
                    rrtype,
                    rrclass,
                    Some(query_record_decoy_reply),
                    remote_query,
                )
            }),
            None => FN_DNSSERVICEQUERYRECORD(
                sd_ref,
                flags,
                interface_index,
                fullname,
                rrtype,
                rrclass,
                callback,
                context,
            ),
        }
    }
}

/// Hook for `DNSServiceQueryRecordWithAttribute` (`DNSServiceQueryRecord` with a
/// `DNSServiceAttribute`, absent on macOS 12), which Bun and libc's own resolver use.
#[allow(non_snake_case)]
#[hook_guard_fn]
unsafe extern "C" fn DNSServiceQueryRecordWithAttribute_detour(
    sd_ref: *mut DNSServiceRef,
    flags: u32,
    interface_index: u32,
    fullname: *const c_char,
    rrtype: u16,
    rrclass: u16,
    attribute: *const c_void,
    callback: Option<DNSServiceQueryRecordReply>,
    context: *mut c_void,
) -> DNSServiceErrorType {
    unsafe {
        let remote_query = callback
            .filter(|_| rrclass == CLASS_IN && matches!(rrtype, TYPE_A | TYPE_AAAA))
            .and_then(|callback| {
                RemoteQuery::new(
                    sd_ref,
                    flags,
                    interface_index,
                    fullname,
                    Query::QueryRecord { callback, rrtype },
                    context,
                )
            });

        match remote_query {
            Some(remote_query) => remote_query.start(sd_ref, |remote_query| {
                FN_DNSSERVICEQUERYRECORDWITHATTRIBUTE(
                    sd_ref,
                    flags,
                    0,
                    DECOY_HOSTNAME.as_ptr(),
                    rrtype,
                    rrclass,
                    attribute,
                    Some(query_record_decoy_reply),
                    remote_query,
                )
            }),
            None => FN_DNSSERVICEQUERYRECORDWITHATTRIBUTE(
                sd_ref,
                flags,
                interface_index,
                fullname,
                rrtype,
                rrclass,
                attribute,
                callback,
                context,
            ),
        }
    }
}

/// Hook for `DNSServiceRefDeallocate`: frees the [`RemoteQuery`] of a query we took over.
/// Deallocating a shared connection also ends the queries started on it.
#[allow(non_snake_case)]
#[hook_guard_fn]
unsafe extern "C" fn DNSServiceRefDeallocate_detour(sd_ref: DNSServiceRef) {
    unsafe {
        FN_DNSSERVICEREFDEALLOCATE(sd_ref);

        let released = {
            let mut registry = registry();
            let sd_ref = sd_ref as usize;
            let mut released: Vec<usize> = registry.queries.remove(&sd_ref).into_iter().collect();

            registry.queries.retain(|_, &mut remote_query| {
                let on_connection = (*(remote_query as *const RemoteQuery)).connection == sd_ref;
                if on_connection {
                    released.push(remote_query);
                }
                !on_connection
            });

            released
        };

        for remote_query in released {
            RemoteQuery::release(remote_query as *mut RemoteQuery);
        }
    }
}

pub(super) unsafe fn enable_dns_sd_hooks(hook_manager: &mut HookManager) {
    unsafe {
        replace!(
            hook_manager,
            "DNSServiceGetAddrInfo",
            DNSServiceGetAddrInfo_detour,
            FnDNSServiceGetAddrInfo,
            FN_DNSSERVICEGETADDRINFO
        );
        replace!(
            hook_manager,
            "DNSServiceGetAddrInfoEx",
            DNSServiceGetAddrInfoEx_detour,
            FnDNSServiceGetAddrInfoEx,
            FN_DNSSERVICEGETADDRINFOEX
        );
        replace!(
            hook_manager,
            "DNSServiceQueryRecord",
            DNSServiceQueryRecord_detour,
            FnDNSServiceQueryRecord,
            FN_DNSSERVICEQUERYRECORD
        );
        replace!(
            hook_manager,
            "DNSServiceQueryRecordWithAttribute",
            DNSServiceQueryRecordWithAttribute_detour,
            FnDNSServiceQueryRecordWithAttribute,
            FN_DNSSERVICEQUERYRECORDWITHATTRIBUTE
        );
        replace!(
            hook_manager,
            "DNSServiceRefDeallocate",
            DNSServiceRefDeallocate_detour,
            FnDNSServiceRefDeallocate,
            FN_DNSSERVICEREFDEALLOCATE
        );
    }
}

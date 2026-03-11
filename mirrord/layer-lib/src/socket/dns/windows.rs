pub mod utils;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Not,
    sync::{LazyLock, Mutex},
};

use socket2::SockAddr;
use utils::{ManagedAddrInfo, ManagedAddrInfoAny, WindowsAddrInfo};
use winapi::{
    shared::ws2def::SOCKADDR,
    um::winsock2::{SOCKET, WSAGetLastError},
};
use windows_strings::PCWSTR;

use crate::{
    detour::{Bypass, Detour, OptionExt},
    error::ConnectError,
    setup::setup,
    socket::dns::remote_getaddrinfo,
};

/// Keep track of managed address info structures for proper cleanup.
/// Maps pointer addresses to the ManagedAddrInfo objects that own them.
pub static MANAGED_ADDRINFO: LazyLock<Mutex<std::collections::HashMap<usize, ManagedAddrInfoAny>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Windows-specific implementation of GetAddrInfo using mirrord's remote DNS resolution.
///
/// This function handles the complete GetAddrInfo workflow including service ports,
/// hints, and DNS selector logic, then uses the trait to convert to the appropriate
/// ADDRINFO structure type. Returns a ManagedAddrInfo that automatically cleans up
/// the ADDRINFO chain when dropped
pub fn getaddrinfo<T: WindowsAddrInfo>(
    raw_node: Option<String>,
    raw_service: Option<String>,
    raw_hints: Option<&T>,
) -> Detour<ManagedAddrInfo<T>> {
    // Convert node to string
    let node = raw_node.bypass(Bypass::NullNode)?;

    // Convert service to port number
    // TODO: according to the man page, service could also be a service name, it doesn't have to
    //   be a port number.
    let port = raw_service.and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);

    tracing::warn!(
        "windows_getaddrinfo called for hostname: {} port: {}",
        node,
        port
    );

    // Check DNS selector to determine if this should be resolved remotely
    setup().dns_selector().check_query(&node, port)?;
    let ipv6_enabled = setup().layer_config().feature.network.ipv6;
    tracing::warn!("Using remote DNS resolution for {}", node);

    let (ai_family, ai_socktype, ai_protocol) = raw_hints
        .map(|hints| hints.get_family_socktype_protocol())
        .unwrap_or((0, 0, 0));

    // Some apps (gRPC on Python) use `::` to listen on all interfaces, and usually that just means
    // resolve on unspecified. So we just return that in IPv4, if IPv6 support is disabled.
    let resolved_addr = if ipv6_enabled.not() && (node == "::") {
        // name is "" because that's what happens in real flow.
        vec![("".to_string(), IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    } else {
        remote_getaddrinfo(node, port, 0, ai_family, ai_socktype, ai_protocol)?
    };

    // Convert response back to Windows ADDRINFO structures using trait method
    let mut managed = ManagedAddrInfo::<T>::try_from(resolved_addr)?;
    managed.apply_port(port);
    Detour::Success(managed)
}

/// Safely deallocates ADDRINFOA structures that were allocated by our getaddrinfo_detour.
///
/// This follows the same pattern as the Unix layer - it checks if the structure
/// was allocated by us (tracked in MANAGED_ADDRINFO) and frees it properly.
///
/// # Safety
/// `addrinfo` must be a pointer that originated from `ManagedAddrInfo` (i.e., from `alloc` plus the
/// bookkeeping map). Passing any other pointer will either fail the lookup or, worse, cause us to
/// free memory we do not own.
pub unsafe fn free_managed_addrinfo<T: WindowsAddrInfo>(addrinfo: *mut T) -> bool {
    let mut managed_addr_info = match MANAGED_ADDRINFO.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("MANAGED_ADDRINFO mutex was poisoned, attempting recovery");
            poisoned.into_inner()
        }
    };

    // Find and remove the managed info by pointer address
    let ptr_address = addrinfo as usize;

    if let Some(_managed_info) = managed_addr_info.remove(&ptr_address) {
        // The Drop implementation of ManagedAddrInfo will handle cleanup automatically
        tracing::debug!("Freed managed ADDRINFO at {:p}", addrinfo);
        true
    } else {
        // Not one of ours
        false
    }
}

/// Helper function to check if a UDP socket's remote address is reachable using GetNameInfoW
/// This is a workaround for WSASend not failing on unreachable addresses (due to UDP being
/// connectionless) Returns:
/// - 0 on success (address is reachable)
/// - Non-zero error code on failure (address unreachable or resolution failed)
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub fn check_address_reachability(socket: SOCKET, remote_addr: &SocketAddr) -> Detour<()> {
    let rawish_sock_addr = SockAddr::from(*remote_addr);
    let sock_addr_len = rawish_sock_addr.len();

    // Buffer for hostname
    let mut node_buffer = [0u16; 256];
    // Buffer for service/port
    let mut service_buffer = [0u16; 32];

    let result = unsafe {
        winapi::um::ws2tcpip::GetNameInfoW(
            rawish_sock_addr.as_ptr() as *const _ as *const SOCKADDR,
            sock_addr_len,
            node_buffer.as_mut_ptr(),
            node_buffer.len() as u32,
            service_buffer.as_mut_ptr(),
            service_buffer.len() as u32,
            //When the NI_NAMEREQD flag is set, a host name that cannot be resolved by the DNS
            // results in an error.
            winapi::shared::ws2def::NI_NAMEREQD,
        )
    };

    if result != 0 {
        tracing::debug!(
            "check_address_reachability -> address resolution failed for socket {} with error {}, wsagetlasterror: {}",
            socket,
            result,
            unsafe { WSAGetLastError() }
        );
        // on failure, GetNameInfoW sets WSALastError
        return Detour::Error(ConnectError::AddressUnreachable(remote_addr.to_string()).into());
    }

    // Successfully resolved - address is reachable
    tracing::debug!(
        "check_address_reachability -> address resolution successful for socket {}: node_buffer: {:?}",
        socket,
        unsafe { str_win::u16_buffer_to_string(PCWSTR(node_buffer.as_ptr()).as_wide()) }
    );

    Detour::Success(())
}

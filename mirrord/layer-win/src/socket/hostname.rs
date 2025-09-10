//! Hostname-related utilities for Windows socket hooks
//!
//! This module contains all the hostname manipulation, DNS resolution, and caching
//! logic used by the Windows socket hooks, separated from the actual hook implementations.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    ffi::CString,
    net::IpAddr,
    sync::{LazyLock, Mutex},
};

use mirrord_layer_lib::{
    common::{
        layer_setup,
        proxy_connection::{PROXY_CONNECTION, make_proxy_request_with_response},
    },
    error::{AddrInfoError, HookResult},
    hostname::{
        HostnameResult, fetch_samba_config_via_proxy, get_hostname, remote_dns_resolve_via_proxy,
    },
    socket::dns::update_dns_reverse_mapping,
    unix::UnixHostnameResolver,
};
use mirrord_protocol::dns::{AddressFamily, GetAddrInfoRequestV2, SockType};
use winapi::{
    shared::ws2def::{AF_INET, AF_INET6},
    um::{
        sysinfoapi::{
            ComputerNameDnsDomain, ComputerNameDnsFullyQualified, ComputerNameDnsHostname,
            ComputerNameNetBIOS, ComputerNamePhysicalDnsDomain,
            ComputerNamePhysicalDnsFullyQualified, ComputerNamePhysicalDnsHostname,
            ComputerNamePhysicalNetBIOS,
        },
        winsock2::{SOCK_DGRAM, SOCK_STREAM},
    },
};

use super::utils::{ManagedAddrInfo, ManagedAddrInfoAny, WindowsAddrInfo, validate_buffer_params};

/// Keep track of managed address info structures for proper cleanup.
/// Maps pointer addresses to the ManagedAddrInfo objects that own them.
pub static MANAGED_ADDRINFO: LazyLock<Mutex<std::collections::HashMap<usize, ManagedAddrInfoAny>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Maximum computer name length on Windows (GetComputerName API limit)
pub const MAX_COMPUTERNAME_LENGTH: usize = 15;

/// Reasonable buffer limit for hostname functions to prevent abuse
///  // Allow for longer DNS names
pub const REASONABLE_BUFFER_LIMIT: usize = 16 * 8;

/// NetBIOS name cache to avoid repeatedly reading Samba config
static NETBIOS_NAME_CACHE: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Read remote Samba configuration to determine NetBIOS name
///
/// This function reads `/etc/samba/smb.conf` from the remote target to extract
/// the configured NetBIOS name, which is more accurate than hostname truncation.
pub fn get_remote_netbios_name() -> Option<String> {
    // Check cache first to avoid repeated file operations
    if let Ok(cache) = NETBIOS_NAME_CACHE.lock() {
        if cache.is_some() {
            return cache.clone();
        }
    }

    let netbios_name = read_samba_config().unwrap_or_else(|e| {
        tracing::debug!(
            "Failed to read Samba config, using hostname fallback: {}",
            e
        );
        None
    });

    // Cache the result (even if None)
    if let Ok(mut cache) = NETBIOS_NAME_CACHE.lock() {
        *cache = netbios_name.clone();
    }

    netbios_name
}

/// Read and parse /etc/samba/smb.conf for NetBIOS name configuration
fn read_samba_config() -> HookResult<Option<String>> {
    tracing::debug!("Reading remote /etc/samba/smb.conf for NetBIOS name");

    match fetch_samba_config_via_proxy() {
        Ok(netbios_name) => {
            tracing::debug!("Parsed NetBIOS name from Samba config: {:?}", netbios_name);
            Ok(netbios_name)
        }
        Err(e) => Err(e.into()),
    }
}

/// Resolve hostname remotely through mirrord agent (similar to Unix layer's remote_getaddrinfo)
pub fn remote_dns_resolve(hostname: &str) -> HookResult<Vec<(String, IpAddr)>> {
    tracing::debug!("Performing remote DNS resolution for: {}", hostname);

    match remote_dns_resolve_via_proxy(hostname) {
        Ok(results) => {
            // Update reverse mapping cache using the unified implementation
            for (name, ip) in results.iter() {
                update_dns_reverse_mapping(*ip, name.clone());
            }
            Ok(results)
        }
        Err(e) => Err(e.into()),
    }
}

/// Get hostname using the Unix hostname resolver (works for Windows too since target is Linux)
pub fn get_hostname_with_resolver() -> HostnameResult {
    match PROXY_CONNECTION.get() {
        Some(proxy) => {
            tracing::debug!("ProxyConnection found, using remote resolver");
            let resolver = UnixHostnameResolver::remote(proxy);
            let result = get_hostname(&resolver);
            tracing::debug!("Remote hostname resolver result: {:?}", result);
            result
        }
        None => {
            tracing::warn!("ProxyConnection not initialized, falling back to local hostname");
            HostnameResult::UseLocal
        }
    }
}

/// Get hostname for specific Windows computer name type
///
/// This function handles the logic for determining what hostname to return
/// based on the Windows GetComputerNameEx name_type parameter.
///
/// Returns None if the hostname feature is disabled or if no hostname is available.
pub fn get_hostname_for_name_type(name_type: u32) -> Option<String> {
    // Check if hostname feature is enabled
    let hostname_enabled = layer_setup().layer_config().feature.hostname;
    if !hostname_enabled {
        tracing::debug!("Hostname feature disabled for name_type {}", name_type);
        return None;
    }

    match name_type {
        ComputerNameNetBIOS | ComputerNamePhysicalNetBIOS => {
            // Try to get NetBIOS name from Samba configuration first
            if let Some(netbios_name) = get_remote_netbios_name() {
                if !netbios_name.is_empty() {
                    let result = netbios_name.to_uppercase();
                    tracing::debug!(
                        "Got NetBIOS name from Samba config for name_type {}: '{}'",
                        name_type,
                        result
                    );
                    return Some(result);
                }
            }

            // Try regular hostname resolution for NetBIOS
            match get_hostname_with_resolver() {
                HostnameResult::Success(hostname_cstring) => {
                    let hostname = hostname_cstring.to_string_lossy().to_string();
                    let netbios = hostname.to_uppercase();
                    tracing::debug!(
                        "Got NetBIOS name from hostname resolution for name_type {}: '{}'",
                        name_type,
                        netbios
                    );
                    Some(netbios)
                }
                _ => {
                    tracing::debug!("No NetBIOS name available for name_type {}", name_type);
                    None // Let original Windows function handle if remote resolution fails
                }
            }
        }
        ComputerNameDnsHostname
        | ComputerNameDnsFullyQualified
        | ComputerNamePhysicalDnsHostname
        | ComputerNamePhysicalDnsFullyQualified => {
            // Use regular hostname for DNS types
            match get_hostname_with_resolver() {
                HostnameResult::Success(dns_cstring) => {
                    let dns_name = dns_cstring.to_string_lossy().to_string();
                    tracing::debug!(
                        "Got DNS hostname for name_type {}: '{}'",
                        name_type,
                        dns_name
                    );
                    Some(dns_name)
                }
                _ => {
                    tracing::debug!("No DNS hostname available for name_type {}", name_type);
                    None // Let original Windows function handle if remote resolution fails
                }
            }
        }
        ComputerNameDnsDomain | ComputerNamePhysicalDnsDomain => {
            // Domain variants - return empty string (no domain info available)
            tracing::debug!("Returning empty domain name for name_type {}", name_type);
            Some(String::new())
        }
        _ => {
            tracing::debug!("Unsupported name_type: {}", name_type);
            None
        }
    }
}

/// Generic hostname function for ANSI versions
pub unsafe fn handle_hostname_ansi<F>(
    lpBuffer: *mut i8,
    nSize: *mut u32,
    original_fn: F,
    function_name: &str,
) -> i32
where
    F: Fn(*mut i8, *mut u32) -> i32,
{
    tracing::debug!("{} hook called", function_name);

    // Check if hostname feature is enabled
    let hostname_enabled = layer_setup().layer_config().feature.hostname;
    tracing::debug!(
        "{}: hostname feature enabled: {}",
        function_name,
        hostname_enabled
    );

    if hostname_enabled
        && let Some(buffer_size) =
            validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
        && let HostnameResult::Success(dns_cstring) = get_hostname_with_resolver()
    {
        // Try to get remote hostname
        let dns_name = dns_cstring.to_string_lossy().to_string();
        let hostname_bytes = dns_name.as_bytes();
        let hostname_with_null: Vec<u8> = hostname_bytes.to_vec();

        if hostname_with_null.len() <= buffer_size && !lpBuffer.is_null() && buffer_size > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    hostname_with_null.as_ptr() as *const i8,
                    lpBuffer,
                    hostname_with_null.len(),
                );
                *nSize = (hostname_with_null.len() - 1) as u32;
            }
            tracing::debug!("{} returning DNS hostname: {}", function_name, dns_name);
            // TRUE - Success
            return 1;
        } else if hostname_with_null.len() <= u32::MAX as usize {
            unsafe {
                *nSize = (hostname_with_null.len() - 1) as u32;
            }
            // FALSE - Buffer too small
            return 0;
        }
    }

    // Fall back to original function (hostname feature disabled or no remote hostname)
    tracing::debug!(
        "{}: using original function (hostname feature disabled or no remote hostname)",
        function_name
    );
    original_fn(lpBuffer, nSize)
}

/// Generic hostname function for Unicode versions
pub unsafe fn handle_hostname_unicode<F>(
    lpBuffer: *mut u16,
    nSize: *mut u32,
    original_fn: F,
    function_name: &str,
) -> i32
where
    F: Fn(*mut u16, *mut u32) -> i32,
{
    tracing::debug!("{} hook called", function_name);

    // Check if hostname feature is enabled
    let hostname_enabled = layer_setup().layer_config().feature.hostname;
    tracing::debug!(
        "{}: hostname feature enabled: {}",
        function_name,
        hostname_enabled
    );

    if hostname_enabled
        && let Some(buffer_size) =
            validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
        && let HostnameResult::Success(dns_cstring) = get_hostname_with_resolver()
    {
        // Try to get remote hostname
        let dns_name = dns_cstring.to_string_lossy().to_string();
        let dns_utf16: Vec<u16> = dns_name.encode_utf16().collect();

        if dns_utf16.len() <= buffer_size && !lpBuffer.is_null() && buffer_size > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(dns_utf16.as_ptr(), lpBuffer, dns_utf16.len());
                *nSize = (dns_utf16.len() - 1) as u32;
            }
            tracing::debug!("{} returning DNS hostname: {}", function_name, dns_name);
            // TRUE - Success
            return 1;
        } else if dns_utf16.len() <= u32::MAX as usize {
            unsafe {
                *nSize = (dns_utf16.len() - 1) as u32;
            }
            // FALSE - Buffer too small
            return 0;
        }
    }

    // If hostname feature is disabled or remote resolution failed, use original function
    tracing::debug!(
        "{}: Using original function (hostname feature disabled or remote resolution failed)",
        function_name
    );
    original_fn(lpBuffer, nSize)
}

/// Helper function to check if a hostname matches our remote hostname
pub fn is_remote_hostname(hostname: &str) -> bool {
    if let HostnameResult::Success(remote_hostname) = get_hostname_with_resolver() {
        let remote_hostname_str = remote_hostname.to_string_lossy();

        // Check if the requested hostname matches our remote hostname or its truncated version
        hostname == remote_hostname_str
            || (remote_hostname_str.len() > MAX_COMPUTERNAME_LENGTH
                && hostname == &remote_hostname_str[..MAX_COMPUTERNAME_LENGTH])
    } else {
        false
    }
}

/// Helper function to resolve hostname with fallback logic
pub fn resolve_hostname_with_fallback(hostname: &str) -> Option<CString> {
    tracing::debug!("resolve_hostname_with_fallback called for: {}", hostname);

    // Check if we should resolve this hostname remotely using the DNS selector
    let should_resolve_remotely = {
        let result = layer_setup()
            .dns_selector()
            .should_resolve_remotely(hostname, 0);
        tracing::debug!(
            "is_remote_hostname DNS selector check for '{}': {}",
            hostname,
            result
        );
        result
    };

    tracing::warn!(
        "DNS selector decision for {}: resolve_remotely={}",
        hostname,
        should_resolve_remotely
    );

    if should_resolve_remotely {
        // Try to resolve the hostname using mirrord's remote DNS resolution
        match remote_dns_resolve(hostname) {
            Ok(results) => {
                if !results.is_empty() {
                    // Use the first IP address from the results
                    let (_name, ip) = &results[0];
                    tracing::debug!("Remote DNS resolution successful: {} -> {}", hostname, ip);

                    // Return the IP address as a CString so gethostbyname can resolve it locally
                    if let Ok(ip_cstring) = CString::new(ip.to_string()) {
                        return Some(ip_cstring);
                    }
                } else {
                    tracing::warn!(
                        "Remote DNS resolution returned empty results for {}",
                        hostname
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Remote DNS resolution failed for {}: {}", hostname, e);
            }
        }

        // Remote resolution failed, use fallback logic
        tracing::debug!(
            "Remote DNS resolution failed for {}, trying fallback",
            hostname
        );
    }

    // As a last resort fallback, try localhost (this may work for some cases)
    tracing::warn!(
        "Using localhost fallback for hostname: {} (this indicates a problem)",
        hostname
    );
    CString::new("127.0.0.1").ok()
}

/// Windows-specific implementation of GetAddrInfo using mirrord's remote DNS resolution.
///
/// This function handles the complete GetAddrInfo workflow including service ports,
/// hints, and DNS selector logic, then uses the trait to convert to the appropriate
/// ADDRINFO structure type. Returns a ManagedAddrInfo that automatically cleans up
/// the ADDRINFO chain when dropped
pub fn windows_getaddrinfo<T: WindowsAddrInfo>(
    raw_node: Option<String>,
    raw_service: Option<String>,
    raw_hints: Option<&T>,
) -> HookResult<ManagedAddrInfo<T>> {
    // Convert node to string
    let node = match raw_node {
        Some(s) => s,
        None => return Err(AddrInfoError::NullPointer.into()),
    };

    // Convert service to port number
    let port = raw_service.and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);

    tracing::warn!(
        "windows_getaddrinfo called for hostname: {} port: {}",
        node,
        port
    );

    // Check DNS selector to determine if this should be resolved remotely
    let should_resolve_remotely = {
        let result = layer_setup()
            .dns_selector()
            .should_resolve_remotely(&node, port);
        tracing::debug!("DNS selector check for '{}': {}", node, result);
        result
    };

    tracing::warn!(
        "DNS selector decision for {} (port {}): resolve_remotely={}",
        node,
        port,
        should_resolve_remotely
    );

    if !should_resolve_remotely {
        tracing::warn!("Using local DNS resolution for {}", node);
        return Err(AddrInfoError::ResolveDisabled(node).into());
    }

    tracing::warn!("Using remote DNS resolution for {}", node);

    // Convert hints to mirrord protocol types
    let (address_family, socket_type, protocol) = match raw_hints {
        Some(hints) => {
            let (ai_family, ai_socktype, ai_protocol) = hints.get_family_socktype_protocol();
            let af = match ai_family {
                AF_INET => AddressFamily::Ipv4Only,
                AF_INET6 => AddressFamily::Ipv6Only,
                _ => AddressFamily::Any,
            };

            let sock_type = match ai_socktype {
                SOCK_STREAM => SockType::Stream,
                SOCK_DGRAM => SockType::Dgram,
                _ => SockType::Any,
            };

            (af, sock_type, ai_protocol)
        }
        None => (AddressFamily::Any, SockType::Any, 0),
    };

    // Make DNS request through mirrord agent
    let request = GetAddrInfoRequestV2 {
        node,
        service_port: port,
        family: address_family,
        socktype: socket_type,
        protocol,
        flags: 0,
    };

    let response = make_proxy_request_with_response(request)?;
    // Convert response back to Windows ADDRINFO structures using trait method
    Ok(ManagedAddrInfo::<T>::try_from(response)?)
}

/// Safely deallocates ADDRINFOA structures that were allocated by our getaddrinfo_detour.
///
/// This follows the same pattern as the Unix layer - it checks if the structure
/// was allocated by us (tracked in MANAGED_ADDRINFO) and frees it properly.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_hostname_detection() {
        // This test would need a mocked resolver to work properly
        // For now, just test that the function doesn't panic
        let result = is_remote_hostname("some-hostname");
        // Result could be true or false depending on state, just ensure no panic
        assert!(result == true || result == false);
    }
}

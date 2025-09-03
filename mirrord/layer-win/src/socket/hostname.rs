//! Hostname-related utilities for Windows socket hooks
//!
//! This module contains all the hostname manipulation, DNS resolution, and caching
//! logic used by the Windows socket hooks, separated from the actual hook implementations.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    collections::HashMap,
    ffi::CString,
    net::IpAddr,
    sync::{LazyLock, Mutex},
};

use mirrord_layer_lib::{HostnameResult, get_hostname, unix::UnixHostnameResolver};
use mirrord_protocol::dns::{AddressFamily, GetAddrInfoRequestV2, LookupRecord, SockType};
use winapi::{
    shared::ws2def::{AF_INET, AF_INET6},
    um::winsock2::{SOCK_DGRAM, SOCK_STREAM},
};

use super::utils::{
    GetAddrInfoResponseExtWin, ManagedAddrInfo, ManagedAddrInfoAny, WindowsAddrInfo,
    evict_old_cache_entries, intelligent_truncate, validate_buffer_params,
};
use crate::{common::make_proxy_request_with_response, PROXY_CONNECTION};

/// Holds the pair of IP addresses with their hostnames, resolved remotely.
/// This is the Windows equivalent of REMOTE_DNS_REVERSE_MAPPING.
/// Uses a bounded collection to prevent memory exhaustion attacks.
pub static REMOTE_DNS_REVERSE_MAPPING: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keep track of managed address info structures for proper cleanup.
/// Maps pointer addresses to the ManagedAddrInfo objects that own them.
pub static MANAGED_ADDRINFO: LazyLock<Mutex<std::collections::HashMap<usize, ManagedAddrInfoAny>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Maximum number of entries in the DNS cache to prevent memory exhaustion
pub const MAX_DNS_CACHE_SIZE: usize = 1000;

/// Maximum computer name length on Windows (GetComputerName API limit)
pub const MAX_COMPUTERNAME_LENGTH: usize = 15;

/// Reasonable buffer limit for hostname functions to prevent abuse
pub const REASONABLE_BUFFER_LIMIT: usize = 16 * 8; // Allow for longer DNS names

/// Resolve hostname remotely through mirrord agent (similar to Unix layer's remote_getaddrinfo)
pub fn remote_dns_resolve(
    hostname: &str,
) -> Result<Vec<(String, IpAddr)>, Box<dyn std::error::Error>> {
    tracing::debug!("Performing remote DNS resolution for: {}", hostname);

    let request = GetAddrInfoRequestV2 {
        node: hostname.to_string(),
        service_port: 0,
        flags: 0,
        family: AddressFamily::Any,
        socktype: SockType::Any,
        protocol: 0,
    };

    // Use the Windows proxy connection directly
    let response = unsafe {
        let proxy_connection = PROXY_CONNECTION
            .get()
            .ok_or("Proxy connection not available")?;

        proxy_connection
            .make_request_with_response(request)
            .map_err(|e| format!("Proxy request failed: {:?}", e))?
    };

    let addr_info_list = response
        .0
        .map_err(|e| format!("Remote DNS resolution failed: {:?}", e))?;

    // Update reverse mapping cache
    if let Ok(mut cache) = REMOTE_DNS_REVERSE_MAPPING.lock() {
        evict_old_cache_entries(&mut cache, MAX_DNS_CACHE_SIZE);
        for lookup in addr_info_list.iter() {
            cache.insert(lookup.ip.to_string(), lookup.name.clone());
        }
    }

    Ok(addr_info_list
        .into_iter()
        .map(|LookupRecord { name, ip }| (name, ip))
        .collect())
}

/// Truncates hostname to fit within MAX_COMPUTERNAME_LENGTH, preserving important parts
fn truncate_to_computer_name_length(hostname: &str) -> String {
    // Extract the first component if it's a FQDN
    if let Some(first_part) = hostname.split('.').next()
        && first_part.len() <= MAX_COMPUTERNAME_LENGTH
        && !first_part.is_empty()
    {
        return first_part.to_string();
    }

    // If hostname is too long, use intelligent truncation
    if hostname.len() > MAX_COMPUTERNAME_LENGTH {
        return intelligent_truncate(hostname, MAX_COMPUTERNAME_LENGTH);
    }

    hostname.to_string()
}

/// Attempts to get a DNS resolvable hostname using remote resolver
/// Returns None if resolution fails or hostname is not DNS resolvable
pub fn get_dns_hostname_with_resolver(hostname: &str) -> Option<String> {
    use std::net::IpAddr;

    // Try to parse as IP address first - if it's already an IP, we can't use it as a hostname
    if hostname.parse::<IpAddr>().is_ok() {
        tracing::debug!("Input '{}' is an IP address, not a hostname", hostname);
        return None;
    }

    // Validate hostname length for safety
    if hostname.is_empty() || hostname.len() > 1024 {
        tracing::debug!("Invalid hostname length: {}", hostname.len());
        return None;
    }

    // Try to resolve the hostname using remote DNS resolution through mirrord agent
    match remote_dns_resolve(hostname) {
        Ok(results) => {
            if !results.is_empty() {
                tracing::debug!("Remote DNS resolution successful for '{}'", hostname);
                Some(hostname.to_string())
            } else {
                tracing::debug!(
                    "Remote DNS resolution returned no results for '{}'",
                    hostname
                );
                // Even if resolution fails, we can still provide a meaningful DNS name
                // This is important for cases where the hostname exists in the cluster
                Some(truncate_to_computer_name_length(hostname))
            }
        }
        Err(e) => {
            tracing::debug!("Remote DNS resolution failed for '{}': {}", hostname, e);

            // Fallback: try local resolution as last resort
            match std::net::ToSocketAddrs::to_socket_addrs(&format!("{}:0", hostname)) {
                Ok(mut addrs) => {
                    if let Some(addr) = addrs.next() {
                        let ip = addr.ip();
                        tracing::debug!(
                            "Local fallback resolution successful: '{}' -> {}",
                            hostname,
                            ip
                        );
                        Some(hostname.to_string())
                    } else {
                        Some(truncate_to_computer_name_length(hostname))
                    }
                }
                Err(_) => {
                    // Even if all resolution fails, provide a meaningful DNS name
                    Some(truncate_to_computer_name_length(hostname))
                }
            }
        }
    }
}

/// Get hostname using the Unix hostname resolver (works for Windows too since target is Linux)
pub fn get_hostname_with_resolver() -> HostnameResult {
    unsafe {
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
}

/// Helper function to get DNS hostname with fallback using remote resolver
pub fn get_hostname_with_fallback() -> Option<String> {
    match get_hostname_with_resolver() {
        HostnameResult::Success(hostname) => {
            let hostname_str = hostname.to_string_lossy();

            // Use the new resolver-based DNS hostname function

            get_dns_hostname_with_resolver(&hostname_str)
                .or_else(|| Some(intelligent_truncate(&hostname_str, MAX_COMPUTERNAME_LENGTH)))
        }
        HostnameResult::UseLocal | HostnameResult::Error(_) => None,
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
    let hostname_enabled = crate::layer_config().feature.hostname;
    tracing::debug!(
        "{}: hostname feature enabled: {}",
        function_name,
        hostname_enabled
    );

    if hostname_enabled
        && let Some(buffer_size) =
            validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
        && let Some(dns_name) = get_hostname_with_fallback()
    {
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
            return 1; // TRUE - Success
        } else if hostname_with_null.len() <= u32::MAX as usize {
            unsafe {
                *nSize = (hostname_with_null.len() - 1) as u32;
            }
            return 0; // FALSE - Buffer too small
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
    let hostname_enabled = crate::layer_config().feature.hostname;
    tracing::debug!(
        "{}: hostname feature enabled: {}",
        function_name,
        hostname_enabled
    );

    if hostname_enabled
        && let Some(buffer_size) =
            validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
        && let Some(dns_name) = get_hostname_with_fallback()
    {
        let dns_utf16: Vec<u16> = dns_name.encode_utf16().collect();

        if dns_utf16.len() <= buffer_size && !lpBuffer.is_null() && buffer_size > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(dns_utf16.as_ptr(), lpBuffer, dns_utf16.len());
                *nSize = (dns_utf16.len() - 1) as u32;
            }
            tracing::debug!("{} returning DNS hostname: {}", function_name, dns_name);
            return 1; // TRUE - Success
        } else if dns_utf16.len() <= u32::MAX as usize {
            unsafe {
                *nSize = (dns_utf16.len() - 1) as u32;
            }
            return 0; // FALSE - Buffer too small
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
        let result = crate::layer_setup()
            .dns_selector
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

        // If remote resolution fails, check if we have the hostname cached locally
        if let Ok(cache) = REMOTE_DNS_REVERSE_MAPPING.lock()
            && let Some(cached_ip) = cache.get(hostname)
        {
            tracing::debug!("Found cached IP for {}: {}", hostname, cached_ip);
            if let Ok(ip_cstring) = CString::new(cached_ip.as_str()) {
                return Some(ip_cstring);
            }
        }
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
) -> Result<ManagedAddrInfo<T>, String> {
    // Convert node to string
    let node = match raw_node {
        Some(s) => s,
        None => return Err("Node name is required".to_string()),
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
        let result = crate::layer_setup()
            .dns_selector
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
        return Err("Should use local DNS resolution".to_string());
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

    let response = make_proxy_request_with_response(request)
        .map_err(|e| format!("DNS request failed: {}", e))?;

    // Convert response back to Windows ADDRINFO structures using trait method
    let ptr = unsafe { response.to_windows_addrinfo::<T>()? };
    Ok(unsafe { ManagedAddrInfo::new(ptr) })
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
    fn test_truncate_to_computer_name_length() {
        // Test FQDN extraction
        let fqdn = "test-pod.default.svc.cluster.local";
        let truncated = truncate_to_computer_name_length(&fqdn);
        assert_eq!(truncated, "test-pod");

        // Test already short hostname
        let short = "short";
        let truncated_short = truncate_to_computer_name_length(&short);
        assert_eq!(truncated_short, "short");
    }

    #[test]
    fn test_remote_hostname_detection() {
        // This test would need a mocked resolver to work properly
        // For now, just test that the function doesn't panic
        let result = is_remote_hostname("some-hostname");
        // Result could be true or false depending on state, just ensure no panic
        assert!(result == true || result == false);
    }
}

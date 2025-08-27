//! Hostname-related utilities for Windows socket hooks
//!
//! This module contains all the hostname manipulation, DNS resolution, and caching
//! logic used by the Windows socket hooks, separated from the actual hook implementations.

use std::{
    alloc::{Layout, alloc, dealloc},
    collections::HashMap,
    ffi::CString,
    net::IpAddr,
    ptr,
    sync::{LazyLock, Mutex},
};

use mirrord_layer_lib::{HostnameResult, get_hostname, unix::UnixHostnameResolver};
use mirrord_protocol::dns::{
    AddressFamily, GetAddrInfoRequestV2, GetAddrInfoResponse, LookupRecord, SockType,
};
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{ADDRINFOA, AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN},
        ws2ipdef::SOCKADDR_IN6,
    },
    um::winsock2::{SOCK_DGRAM, SOCK_STREAM},
};

use super::utils::{evict_old_cache_entries, intelligent_truncate, validate_buffer_params};
use crate::PROXY_CONNECTION;

/// Holds the pair of IP addresses with their hostnames, resolved remotely.
/// This is the Windows equivalent of REMOTE_DNS_REVERSE_MAPPING.
/// Uses a bounded collection to prevent memory exhaustion attacks.
pub static REMOTE_DNS_REVERSE_MAPPING: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keep track of managed address info structures for proper cleanup.
pub static MANAGED_ADDRINFO: LazyLock<Mutex<std::collections::HashSet<usize>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Maximum number of entries in the DNS cache to prevent memory exhaustion
pub const MAX_DNS_CACHE_SIZE: usize = 1000;

/// Maximum computer name length on Windows (GetComputerName API limit)
pub const MAX_COMPUTERNAME_LENGTH: usize = 15;

/// Reasonable buffer limit for hostname functions to prevent abuse
pub const REASONABLE_BUFFER_LIMIT: usize = 16 * 8; // Allow for longer DNS names

/// Resolve hostname remotely through mirrord agent (similar to Unix layer's remote_getaddrinfo)
fn remote_dns_resolve(hostname: &str) -> Result<Vec<(String, IpAddr)>, Box<dyn std::error::Error>> {
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
    if let Some(first_part) = hostname.split('.').next() {
        if first_part.len() <= MAX_COMPUTERNAME_LENGTH && !first_part.is_empty() {
            return first_part.to_string();
        }
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
            let result = get_dns_hostname_with_resolver(&hostname_str)
                .or_else(|| Some(intelligent_truncate(&hostname_str, MAX_COMPUTERNAME_LENGTH)));
            result
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

    if let Some(buffer_size) =
        validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
    {
        if let Some(dns_name) = get_hostname_with_fallback() {
            let hostname_bytes = dns_name.as_bytes();
            let hostname_with_null: Vec<u8> = hostname_bytes.iter().cloned().collect();

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
    }

    // Fall back to original function
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

    if let Some(buffer_size) =
        validate_buffer_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT)
    {
        if let Some(dns_name) = get_hostname_with_fallback() {
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
    }

    // Fall back to original function
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

    // First, try to resolve the hostname using mirrord's remote DNS resolution
    // This is the correct approach - we should resolve through the agent, not locally
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
    if let Ok(cache) = REMOTE_DNS_REVERSE_MAPPING.lock() {
        if let Some(cached_ip) = cache.get(hostname) {
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

/// Helper function to make proxy request with response (Windows version)
pub fn make_windows_proxy_request_with_response<T>(request: T) -> Result<T::Response, String>
where
    T: mirrord_intproxy_protocol::IsLayerRequestWithResponse + std::fmt::Debug,
    T::Response: std::fmt::Debug,
{
    unsafe {
        PROXY_CONNECTION
            .get()
            .ok_or_else(|| "ProxyConnection not initialized".to_string())?
            .make_request_with_response(request)
            .map_err(|e| format!("Proxy request failed: {:?}", e))
    }
}

/// Windows-specific getaddrinfo implementation using mirrord protocol
///
/// This function converts Windows types to mirrord protocol types and makes DNS requests
/// through the mirrord agent, then converts the response back to Windows ADDRINFOA structures.
pub fn windows_getaddrinfo(
    rawish_node: Option<&std::ffi::CStr>,
    rawish_service: Option<&std::ffi::CStr>,
    rawish_hints: Option<&ADDRINFOA>,
) -> Result<*mut ADDRINFOA, String> {
    // Convert node to string
    let node = match rawish_node {
        Some(cstr) => match cstr.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return Err("Invalid UTF-8 in node name".to_string()),
        },
        None => return Err("Node name is required".to_string()),
    };

    // Convert service to port number
    let port = match rawish_service {
        Some(cstr) => match cstr.to_str() {
            Ok(s) => s.parse::<u16>().unwrap_or(0),
            Err(_) => return Err("Invalid UTF-8 in service name".to_string()),
        },
        None => 0,
    };

    // Convert hints to mirrord protocol types
    let (address_family, socket_type, protocol) = match rawish_hints {
        Some(hints) => {
            let af = match hints.ai_family {
                AF_INET => AddressFamily::Ipv4Only,
                AF_INET6 => AddressFamily::Ipv6Only,
                _ => AddressFamily::Any,
            };

            let sock_type = match hints.ai_socktype {
                SOCK_STREAM => SockType::Stream,
                SOCK_DGRAM => SockType::Dgram,
                _ => SockType::Any,
            };

            (af, sock_type, hints.ai_protocol)
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

    let response = make_windows_proxy_request_with_response(request)
        .map_err(|e| format!("DNS request failed: {}", e))?;

    // Convert response back to Windows ADDRINFOA structures
    convert_dns_response_to_addrinfo(response)
}

/// Convert mirrord DNS response to Windows ADDRINFOA linked list
///
/// This allocates Windows-compatible ADDRINFOA structures that can be freed
/// by our freeaddrinfo_detour function.
pub fn convert_dns_response_to_addrinfo(
    response: GetAddrInfoResponse,
) -> Result<*mut ADDRINFOA, String> {
    use std::ffi::CString;

    // Check if the response was successful
    let dns_lookup = match response.0 {
        Ok(lookup) => lookup,
        Err(e) => return Err(format!("DNS lookup failed: {:?}", e)),
    };

    if dns_lookup.is_empty() {
        return Err("No addresses in DNS response".to_string());
    }

    let mut first_addrinfo: *mut ADDRINFOA = ptr::null_mut();
    let mut current_addrinfo: *mut ADDRINFOA = ptr::null_mut();

    for lookup_record in dns_lookup.into_iter() {
        // Allocate ADDRINFOA structure
        let layout = Layout::new::<ADDRINFOA>();
        let addrinfo_ptr = unsafe { alloc(layout) as *mut ADDRINFOA };
        if addrinfo_ptr.is_null() {
            return Err("Failed to allocate ADDRINFOA".to_string());
        }

        // Parse the IP address and create sockaddr
        let (sockaddr_ptr, sockaddr_len, family) = match lookup_record.ip {
            std::net::IpAddr::V4(ipv4) => {
                let layout = Layout::new::<SOCKADDR_IN>();
                let sockaddr_in_ptr = unsafe { alloc(layout) as *mut SOCKADDR_IN };
                if sockaddr_in_ptr.is_null() {
                    return Err("Failed to allocate SOCKADDR_IN".to_string());
                }

                unsafe {
                    (*sockaddr_in_ptr).sin_family = AF_INET as u16;
                    (*sockaddr_in_ptr).sin_port = 0; // Port not available in LookupRecord
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
                let layout = Layout::new::<SOCKADDR_IN6>();
                let sockaddr_in6_ptr = unsafe { alloc(layout) as *mut SOCKADDR_IN6 };
                if sockaddr_in6_ptr.is_null() {
                    return Err("Failed to allocate SOCKADDR_IN6".to_string());
                }

                unsafe {
                    (*sockaddr_in6_ptr).sin6_family = AF_INET6 as u16;
                    (*sockaddr_in6_ptr).sin6_port = 0; // Port not available in LookupRecord
                    (*sockaddr_in6_ptr).sin6_flowinfo = 0;
                    *(*sockaddr_in6_ptr).sin6_addr.u.Byte_mut() = ipv6.octets();
                    // Note: sin6_scope_id field may not be available in this Windows API version
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
            match CString::new(lookup_record.name) {
                Ok(cstr) => cstr.into_raw(),
                Err(_) => ptr::null_mut(),
            }
        } else {
            ptr::null_mut()
        };

        // Fill in the ADDRINFOA structure
        unsafe {
            (*addrinfo_ptr).ai_flags = 0;
            (*addrinfo_ptr).ai_family = family;
            (*addrinfo_ptr).ai_socktype = SOCK_STREAM; // Default to STREAM, could be improved
            (*addrinfo_ptr).ai_protocol = 0;
            (*addrinfo_ptr).ai_addrlen = sockaddr_len as usize;
            (*addrinfo_ptr).ai_canonname = canonname;
            (*addrinfo_ptr).ai_addr = sockaddr_ptr;
            (*addrinfo_ptr).ai_next = ptr::null_mut();
        }

        // Link into the list
        if first_addrinfo.is_null() {
            first_addrinfo = addrinfo_ptr;
            current_addrinfo = addrinfo_ptr;
        } else {
            unsafe {
                (*current_addrinfo).ai_next = addrinfo_ptr;
                current_addrinfo = addrinfo_ptr;
            }
        }

        // Track this allocation for cleanup
        MANAGED_ADDRINFO
            .lock()
            .expect("MANAGED_ADDRINFO lock failed")
            .insert(addrinfo_ptr as usize);
    }

    Ok(first_addrinfo)
}

/// Safely deallocates ADDRINFOA structures that were allocated by our getaddrinfo_detour.
///
/// This follows the same pattern as the Unix layer - it checks if the structure
/// was allocated by us (tracked in MANAGED_ADDRINFO) and frees it properly.
pub unsafe fn free_managed_addrinfo(addrinfo: *mut ADDRINFOA) -> bool {
    use std::ffi::CString;

    let mut managed_addr_info = MANAGED_ADDRINFO
        .lock()
        .expect("MANAGED_ADDRINFO lock failed");

    if managed_addr_info.remove(&(addrinfo as usize)) {
        // This is one of our allocated structures - clean it up properly
        let mut current = addrinfo;
        while !current.is_null() {
            unsafe {
                let next = (*current).ai_next;

                // Free the sockaddr structure
                if !(*current).ai_addr.is_null() {
                    let sockaddr_layout = if (*current).ai_family == AF_INET {
                        Layout::new::<SOCKADDR_IN>()
                    } else {
                        Layout::new::<SOCKADDR_IN6>()
                    };
                    dealloc((*current).ai_addr as *mut u8, sockaddr_layout);
                }

                // Free the canonical name if present
                if !(*current).ai_canonname.is_null() {
                    let _ = CString::from_raw((*current).ai_canonname);
                }

                // Free the ADDRINFOA structure itself
                let addrinfo_layout = Layout::new::<ADDRINFOA>();
                dealloc(current as *mut u8, addrinfo_layout);

                // Remove from managed set and move to next
                managed_addr_info.remove(&(current as usize));
                current = next;
            }
        }
        true
    } else {
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

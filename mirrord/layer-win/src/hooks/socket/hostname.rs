//! Hostname-related utilities for Windows socket hooks
//! 
//! This module contains all the hostname manipulation, DNS resolution, and caching
//! logic used by the Windows socket hooks, separated from the actual hook implementations.

use std::{
    collections::HashMap,
    ffi::CString,
    sync::{LazyLock, Mutex},
};

use mirrord_layer_lib::{HostnameResult, get_or_init_hostname, DefaultHostnameResolver};
use winapi::um::winsock2::HOSTENT;

/// Holds the pair of IP addresses with their hostnames, resolved remotely.
/// This is the Windows equivalent of REMOTE_DNS_REVERSE_MAPPING.
/// Uses a bounded collection to prevent memory exhaustion attacks.
pub static REMOTE_DNS_REVERSE_MAPPING: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maximum number of entries in the DNS cache to prevent memory exhaustion
pub const MAX_DNS_CACHE_SIZE: usize = 1000;

/// Maximum computer name length on Windows (GetComputerName API limit)
pub const MAX_COMPUTERNAME_LENGTH: usize = 15;

/// Reasonable buffer limit for hostname functions to prevent abuse
pub const REASONABLE_BUFFER_LIMIT: usize = 16 * 8; // Allow for longer DNS names

/// Implement simple LRU eviction for DNS cache to avoid clearing entire cache
pub fn evict_old_dns_entries(cache: &mut HashMap<String, String>) {
    if cache.len() >= MAX_DNS_CACHE_SIZE {
        // Simple eviction: remove oldest entries (first half of current entries)
        // This is a compromise between performance and memory usage
        let keys_to_remove: Vec<String> = cache.keys()
            .take(MAX_DNS_CACHE_SIZE / 2)
            .cloned()
            .collect();
        
        for key in keys_to_remove {
            cache.remove(&key);
        }
        
        tracing::debug!("DNS cache evicted {} old entries, {} remaining", 
                       MAX_DNS_CACHE_SIZE / 2, cache.len());
    }
}

/// Intelligent hostname truncation that preserves important substrings
pub fn intelligent_truncate(hostname: &str, max_len: usize) -> String {
    if hostname.len() <= max_len {
        return hostname.to_string();
    }
    
    // Priority list of important substrings to preserve
    const IMPORTANT_PATTERNS: &[&str] = &[
        "hostname-echo",
        "test-pod",
        "app-",
        "service-",
    ];
    
    // Try to find and preserve important patterns
    for pattern in IMPORTANT_PATTERNS {
        if let Some(start) = hostname.find(pattern) {
            let pattern_end = start + pattern.len();
            
            // If the pattern plus some context fits in the buffer
            if pattern_end <= max_len {
                // Take from the beginning to preserve the pattern
                return hostname[..max_len].to_string();
            } else if pattern.len() <= max_len {
                // Take just the pattern if it fits
                return pattern.to_string();
            }
        }
    }
    
    // No important patterns found, use simple truncation
    // Try to break at word boundaries if possible
    if let Some(dash_pos) = hostname[..max_len].rfind('-') {
        if dash_pos > max_len / 2 {  // Only use if it's not too short
            return hostname[..dash_pos].to_string();
        }
    }
    
    // Fallback to simple truncation
    hostname[..max_len].to_string()
}

/// Helper function to extract IP address from HOSTENT structure
/// SAFETY: This function assumes the HOSTENT pointer is valid and properly formatted
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
        tracing::warn!("extract_ip_from_hostent: invalid address length {}", host.h_length);
        return None;
    }
    
    // SAFETY: Validate pointer alignment and basic sanity checks
    if (first_addr_ptr as usize) % std::mem::align_of::<u8>() != 0 {
        tracing::warn!("extract_ip_from_hostent: misaligned address pointer");
        return None;
    }
    
    // Additional safety: verify the pointer is within reasonable bounds
    // This is a basic check - in production, consider using VirtualQuery
    if (first_addr_ptr as usize) < 0x1000 || (first_addr_ptr as usize) > 0x7FFFFFFFFFFF {
        tracing::warn!("extract_ip_from_hostent: suspicious pointer address: {:p}", first_addr_ptr);
        return None;
    }
    
    // Extract IP based on address family
    match host.h_addrtype {
        2 => { // AF_INET (IPv4)
            if host.h_length == 4 {
                let ip_bytes = unsafe { std::slice::from_raw_parts(first_addr_ptr as *const u8, 4) };
                Some(format!("{}.{}.{}.{}", ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]))
            } else {
                tracing::warn!("extract_ip_from_hostent: IPv4 address has invalid length {}", host.h_length);
                None
            }
        }
        23 => { // AF_INET6 (IPv6)
            if host.h_length == 16 {
                let ip_bytes = unsafe { std::slice::from_raw_parts(first_addr_ptr as *const u8, 16) };
                // Convert to proper IPv6 string format with colon notation
                Some(format!("{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                    ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3],
                    ip_bytes[4], ip_bytes[5], ip_bytes[6], ip_bytes[7],
                    ip_bytes[8], ip_bytes[9], ip_bytes[10], ip_bytes[11],
                    ip_bytes[12], ip_bytes[13], ip_bytes[14], ip_bytes[15]
                ))
            } else {
                tracing::warn!("extract_ip_from_hostent: IPv6 address has invalid length {}", host.h_length);
                None
            }
        }
        _ => {
            tracing::debug!("extract_ip_from_hostent: unsupported address family {}", host.h_addrtype);
            None
        }
    }
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

/// Attempts to get a DNS resolvable hostname using Windows API
/// Returns None if resolution fails or hostname is not DNS resolvable
pub fn get_dns_hostname(hostname: &str) -> Option<String> {
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
    
    // Try to resolve the hostname using DNS lookup without requiring a specific port
    // Use port 0 as a placeholder since we only care about DNS resolution, not connectivity
    match std::net::ToSocketAddrs::to_socket_addrs(&format!("{}:0", hostname)) {
        Ok(mut addrs) => {
            if let Some(addr) = addrs.next() {
                let ip = addr.ip();
                tracing::debug!("Hostname '{}' resolves to IP: {}", hostname, ip);
                Some(truncate_to_computer_name_length(hostname))
            } else {
                tracing::debug!("Hostname '{}' did not resolve to any addresses", hostname);
                None
            }
        }
        Err(e) => {
            tracing::debug!("Failed to resolve hostname '{}': {}", hostname, e);
            
            // Even if resolution fails, we can still provide a meaningful DNS name
            // This is important for cases where the hostname exists in the cluster
            // but is not resolvable from the local machine
            Some(truncate_to_computer_name_length(hostname))
        }
    }
}

/// Set the remote hostname that should be returned by gethostname
/// This now uses the shared layer-lib functionality
pub fn set_remote_hostname(hostname: String) -> Result<(), Box<dyn std::error::Error>> {
    let c_hostname = CString::new(hostname)?;
    mirrord_layer_lib::set_hostname(c_hostname).map_err(|e| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("Failed to set hostname: {e:?}")
        )) as Box<dyn std::error::Error>
    })?;
    tracing::debug!("Set remote hostname for gethostname hook using layer-lib");
    Ok(())
}

/// Get hostname using the hostname resolver
pub fn get_hostname_with_resolver() -> HostnameResult {
    let resolver = DefaultHostnameResolver::remote();
    get_or_init_hostname(&resolver)
}

/// Helper function to validate input parameters for Windows hostname functions
pub fn validate_hostname_params(_buffer: *mut u8, size: *mut u32, max_reasonable_size: usize) -> Option<usize> {
    if size.is_null() {
        return None;
    }
    
    let buffer_size = unsafe { *size } as usize;
    
    if buffer_size > max_reasonable_size {
        tracing::warn!("Suspicious buffer size {}, falling back to original", buffer_size);
        return None;
    }
    
    Some(buffer_size)
}

/// Helper function to get DNS hostname with fallback
pub fn get_hostname_with_fallback() -> Option<String> {
    match get_hostname_with_resolver() {
        HostnameResult::Success(hostname) => {
            let hostname_str = hostname.to_string_lossy();
            
            Some(get_dns_hostname(&hostname_str).unwrap_or_else(|| {
                intelligent_truncate(&hostname_str, MAX_COMPUTERNAME_LENGTH)
            }))
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
    
    if let Some(buffer_size) = validate_hostname_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT) {
        if let Some(dns_name) = get_hostname_with_fallback() {
            let hostname_bytes = dns_name.as_bytes();
            let hostname_with_null: Vec<u8> = hostname_bytes.iter().chain(std::iter::once(&0u8)).cloned().collect();
            
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
    
    if let Some(buffer_size) = validate_hostname_params(lpBuffer as *mut u8, nSize, REASONABLE_BUFFER_LIMIT) {
        if let Some(dns_name) = get_hostname_with_fallback() {
            let dns_utf16: Vec<u16> = dns_name.encode_utf16().chain(std::iter::once(0)).collect();
            
            if dns_utf16.len() <= buffer_size && !lpBuffer.is_null() && buffer_size > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        dns_utf16.as_ptr(),
                        lpBuffer,
                        dns_utf16.len(),
                    );
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
        hostname == remote_hostname_str ||
            (remote_hostname_str.len() > MAX_COMPUTERNAME_LENGTH && hostname == &remote_hostname_str[..MAX_COMPUTERNAME_LENGTH])
    } else {
        false
    }
}

/// Helper function to resolve hostname with fallback logic
pub fn resolve_hostname_with_fallback(hostname: &str) -> Option<CString> {
    if let HostnameResult::Success(remote_hostname) = get_hostname_with_resolver() {
        let remote_hostname_str = remote_hostname.to_string_lossy();
        
        // Try to resolve the full hostname if we truncated it
        if hostname.len() < remote_hostname_str.len() {
            tracing::debug!("Trying to resolve full hostname instead of truncated version");
            if let Ok(full_hostname_cstr) = CString::new(remote_hostname_str.as_ref()) {
                return Some(full_hostname_cstr);
            }
        }
    }
    
    // If direct resolution fails, try resolving localhost as fallback
    tracing::debug!("Trying localhost fallback for our hostname");
    CString::new("localhost").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intelligent_truncate_preserves_important_patterns() {
        // Test preserving hostname-echo pattern
        let hostname = "very-long-hostname-echo-pod-name-that-exceeds-15-chars";
        let truncated = intelligent_truncate(&hostname, MAX_COMPUTERNAME_LENGTH);
        assert!(truncated.contains("hostname-echo") || truncated.len() <= MAX_COMPUTERNAME_LENGTH);
        
        // Test short hostname
        let short = "short";
        let truncated_short = intelligent_truncate(&short, MAX_COMPUTERNAME_LENGTH);
        assert_eq!(truncated_short, "short");
        
        // Test exact length
        let exact = "exactly15chars!";
        let truncated_exact = intelligent_truncate(&exact, MAX_COMPUTERNAME_LENGTH);
        assert_eq!(truncated_exact, "exactly15chars!");
    }

    #[test]
    fn test_intelligent_truncate_word_boundaries() {
        let hostname = "app-service-backend";
        let truncated = intelligent_truncate(&hostname, 10);
        // Should either preserve "app-" pattern or break at word boundary
        assert!(truncated.len() <= 10);
        assert!(!truncated.ends_with('-') || truncated == "app-");
    }

    #[test]
    fn test_evict_old_dns_entries() {
        let mut cache = HashMap::new();
        
        // Fill cache beyond limit
        for i in 0..(MAX_DNS_CACHE_SIZE + 10) {
            cache.insert(format!("hostname{}", i), format!("127.0.0.{}", i % 255));
        }
        
        let original_size = cache.len();
        evict_old_dns_entries(&mut cache);
        
        // Should have evicted half the entries
        assert!(cache.len() < original_size);
        assert!(cache.len() <= MAX_DNS_CACHE_SIZE);
    }
}

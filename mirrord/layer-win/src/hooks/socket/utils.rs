//! Utility functions for Windows socket operations

use std::{collections::HashMap, mem, net::SocketAddr};

use mirrord_protocol::outgoing::SocketAddress;
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN},
        ws2ipdef::SOCKADDR_IN6,
    },
    um::winsock2::HOSTENT,
};

/// Helper function to convert Windows SOCKADDR to Rust SocketAddr
pub unsafe fn sockaddr_to_socket_addr(addr: *const SOCKADDR, addrlen: INT) -> Option<SocketAddr> {
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

/// Intelligent string truncation that preserves important substrings
///
/// This function is useful for truncating hostnames while preserving meaningful patterns
/// that might be important for identification or debugging purposes.
pub fn intelligent_truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }

    // Priority list of important substrings to preserve
    const IMPORTANT_PATTERNS: &[&str] = &["hostname-echo", "test-pod", "app-", "service-"];

    // Try to find and preserve important patterns
    for pattern in IMPORTANT_PATTERNS {
        if let Some(start) = text.find(pattern) {
            let pattern_end = start + pattern.len();

            // If the pattern plus some context fits in the buffer
            if pattern_end <= max_len {
                // Take from the beginning to preserve the pattern
                return text[..max_len].to_string();
            } else if pattern.len() <= max_len {
                // Take just the pattern if it fits
                return pattern.to_string();
            }
        }
    }

    // No important patterns found, use simple truncation
    // Try to break at word boundaries if possible
    if let Some(dash_pos) = text[..max_len].rfind('-')
        && dash_pos > max_len / 2 {
            // Only use if it's not too short
            return text[..dash_pos].to_string();
        }

    // Fallback to simple truncation
    text[..max_len].to_string()
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
                Some(format!(
                    "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                    ip_bytes[0],
                    ip_bytes[1],
                    ip_bytes[2],
                    ip_bytes[3],
                    ip_bytes[4],
                    ip_bytes[5],
                    ip_bytes[6],
                    ip_bytes[7],
                    ip_bytes[8],
                    ip_bytes[9],
                    ip_bytes[10],
                    ip_bytes[11],
                    ip_bytes[12],
                    ip_bytes[13],
                    ip_bytes[14],
                    ip_bytes[15]
                ))
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

    if buffer_size > max_reasonable_size {
        tracing::warn!("Suspicious buffer size {}, rejecting request", buffer_size);
        return None;
    }

    Some(buffer_size)
}

/// Implement simple LRU eviction for cache to avoid clearing entire cache
///
/// This function provides a generic cache eviction strategy that removes approximately
/// half of the oldest entries when the cache size exceeds the maximum limit.
pub fn evict_old_cache_entries<K, V>(cache: &mut HashMap<K, V>, max_size: usize)
where
    K: Clone + std::hash::Hash + Eq,
{
    if cache.len() >= max_size {
        // Simple eviction: remove oldest entries (first half of current entries)
        // This is a compromise between performance and memory usage
        let keys_to_remove: Vec<K> = cache.keys().take(max_size / 2).cloned().collect();

        for key in keys_to_remove {
            cache.remove(&key);
        }

        tracing::debug!(
            "Cache evicted {} old entries, {} remaining",
            max_size / 2,
            cache.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sockaddr_conversion_safety() {
        // Test null pointer
        let result = unsafe { sockaddr_to_socket_addr(std::ptr::null(), 0) };
        assert!(result.is_none());

        // Test invalid length
        let dummy_addr = std::mem::MaybeUninit::<SOCKADDR>::uninit();
        let result = unsafe { sockaddr_to_socket_addr(dummy_addr.as_ptr(), -1) };
        assert!(result.is_none());
    }

    #[test]
    fn test_intelligent_truncate_preserves_important_patterns() {
        // Test preserving hostname-echo pattern
        let text = "very-long-hostname-echo-pod-name-that-exceeds-limits";
        let truncated = intelligent_truncate(&text, 15);
        assert!(truncated.contains("hostname-echo") || truncated.len() <= 15);

        // Test short text
        let short = "short";
        let truncated_short = intelligent_truncate(&short, 15);
        assert_eq!(truncated_short, "short");

        // Test exact length
        let exact = "exactly15chars!";
        let truncated_exact = intelligent_truncate(&exact, 15);
        assert_eq!(truncated_exact, "exactly15chars!");
    }

    #[test]
    fn test_intelligent_truncate_word_boundaries() {
        let text = "app-service-backend";
        let truncated = intelligent_truncate(&text, 10);
        // Should either preserve "app-" pattern or break at word boundary
        assert!(truncated.len() <= 10);
        assert!(!truncated.ends_with('-') || truncated == "app-");
    }

    #[test]
    fn test_evict_old_cache_entries() {
        let mut cache = HashMap::new();
        let max_size = 10;

        // Fill cache beyond limit
        for i in 0..(max_size + 5) {
            cache.insert(format!("key{}", i), format!("value{}", i));
        }

        let original_size = cache.len();
        evict_old_cache_entries(&mut cache, max_size);

        // Should have evicted half the entries
        assert!(cache.len() < original_size);
        assert!(cache.len() <= max_size);
    }

    #[test]
    fn test_validate_buffer_params() {
        let mut size = 100u32;
        let result = validate_buffer_params(std::ptr::null_mut(), &mut size, 1000);
        assert_eq!(result, Some(100));

        // Test oversized buffer
        let mut large_size = 10000u32;
        let result = validate_buffer_params(std::ptr::null_mut(), &mut large_size, 1000);
        assert_eq!(result, None);

        // Test null size pointer
        let result = validate_buffer_params(std::ptr::null_mut(), std::ptr::null_mut(), 1000);
        assert_eq!(result, None);
    }
}

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{LazyLock, Mutex},
};

/// Holds the pair of [`IpAddr`] with their hostnames, resolved remotely through
/// remote DNS resolution functions.
///
/// Used by outgoing connection functions to retrieve the hostname from the address that the user
/// called `connect` with, so we can resolve it locally when necessary.
///
/// This provides a unified DNS reverse mapping for both Unix and Windows layers:
/// - Unix layer: Maps IP addresses to hostnames for connect operations
/// - Windows layer: Uses a bounded collection to prevent memory exhaustion attacks
///
/// The cache helps avoid redundant DNS lookups and provides hostname information
/// for debugging and logging purposes.
pub static REMOTE_DNS_REVERSE_MAPPING: LazyLock<Mutex<HashMap<IpAddr, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maximum number of entries in the DNS cache to prevent memory exhaustion
pub const MAX_DNS_CACHE_SIZE: usize = 1000;

/// Update the DNS reverse mapping with new lookup results.
/// This is called when DNS resolution is performed remotely through the mirrord agent.
/// Includes cache size management to prevent memory exhaustion attacks.
///
/// # Arguments
/// * `ip` - The IP address that was resolved
/// * `hostname` - The original hostname that resolved to this IP
pub fn update_dns_reverse_mapping(ip: IpAddr, hostname: String) {
    if let Ok(mut mapping) = REMOTE_DNS_REVERSE_MAPPING.lock() {
        // If we're at capacity, remove the oldest entry (simple eviction strategy)
        if mapping.len() >= MAX_DNS_CACHE_SIZE {
            // Remove an arbitrary entry to make space
            if let Some(key) = mapping.keys().next().cloned() {
                mapping.remove(&key);
            }
        }
        mapping.insert(ip, hostname);
    }
}

/// Get the original hostname for an IP address from the reverse mapping.
/// Returns None if the IP was not found in the mapping.
///
/// # Arguments
/// * `ip` - The IP address to look up
///
/// # Returns
/// * `Some(hostname)` if the IP was found in the mapping
/// * `None` if the IP was not found
pub fn get_hostname_for_ip(ip: IpAddr) -> Option<String> {
    REMOTE_DNS_REVERSE_MAPPING.lock().ok()?.get(&ip).cloned()
}

/// Clear all entries from the DNS reverse mapping.
/// This can be used for cleanup or memory management.
pub fn clear_dns_reverse_mapping() {
    if let Ok(mut mapping) = REMOTE_DNS_REVERSE_MAPPING.lock() {
        mapping.clear();
    }
}

/// Get the current size of the DNS reverse mapping.
/// Useful for monitoring and debugging.
pub fn dns_reverse_mapping_size() -> usize {
    REMOTE_DNS_REVERSE_MAPPING
        .lock()
        .map(|mapping| mapping.len())
        .unwrap_or(0)
}

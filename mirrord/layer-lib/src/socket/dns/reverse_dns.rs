use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::LazyLock,
};

use crate::mutex::Mutex;

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
const MAX_DNS_CACHE_SIZE: usize = 1000;

/// Update the DNS reverse mapping with new lookup results.
/// This is called when DNS resolution is performed remotely through the mirrord agent.
/// Includes cache size management to prevent memory exhaustion attacks.
///
/// # Arguments
/// * `ip` - The IP address that was resolved
/// * `hostname` - The original hostname that resolved to this IP
pub fn update_dns_reverse_mapping_bulk(lookups: &Vec<(String, IpAddr)>) {
    if lookups.is_empty() {
        return;
    }

    if let Ok(mut mapping) = REMOTE_DNS_REVERSE_MAPPING.lock() {
        // Count how many *new* IPs we'll be adding so we can evict up-front.
        let mut new_ips = HashSet::new();
        for (_, ip) in lookups {
            if !mapping.contains_key(ip) {
                new_ips.insert(*ip);
            }
        }

        if !new_ips.is_empty() {
            // Figure out how much space is missing.
            let available_slots = MAX_DNS_CACHE_SIZE.saturating_sub(mapping.len());
            let mut to_evict = new_ips.len().saturating_sub(available_slots);

            // Evict entries ahead of time so new inserts are safe.
            while to_evict > 0 && !mapping.is_empty() {
                if let Some(key) = mapping.keys().next().cloned() {
                    mapping.remove(&key);
                    to_evict -= 1;
                } else {
                    break;
                }
            }
        }

        // Apply the batch once enough space is guaranteed.
        for (hostname, ip) in lookups {
            mapping.insert(*ip, hostname.clone());
        }
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

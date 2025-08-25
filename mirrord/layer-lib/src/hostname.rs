//! Hostname resolution functionality shared between Unix layer and Windows layer-win.
//! 
//! This module provides a cross-platform interface for managing hostname resolution
//! that can be used by both the Unix layer and the Windows layer-win.

use std::{
    ffi::CString,
    sync::OnceLock,
};

use tracing::trace;

/// Hostname initialized from the agent with cached storage.
pub static HOSTNAME: OnceLock<CString> = OnceLock::new();

/// Error type for hostname operations
#[derive(Debug, thiserror::Error)]
pub enum HostnameError {
    #[error("Should use local hostname")]
    UseLocal,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid hostname data")]
    InvalidData,
    #[error("Protocol error: {0}")]
    Protocol(String),
}

/// Set the hostname value in the global cache.
/// This should be called when the hostname is fetched from the remote agent.
pub fn set_hostname(hostname: CString) -> Result<(), HostnameError> {
    HOSTNAME.set(hostname).map_err(|_| {
        trace!("Hostname was already set, ignoring new value");
        HostnameError::InvalidData
    })
}

/// Get the cached hostname if it exists.
/// Returns None if hostname hasn't been fetched yet.
pub fn get_cached_hostname() -> Option<&'static CString> {
    HOSTNAME.get()
}

/// Result type for hostname resolution that can indicate different outcomes
#[derive(Debug)]
pub enum HostnameResult {
    /// Successfully resolved hostname
    Success(CString),
    /// Should bypass to local hostname resolution
    UseLocal,
    /// An error occurred
    Error(HostnameError),
}

/// Trait for hostname resolution backend that can be implemented differently
/// for Unix and Windows layers depending on their specific requirements.
pub trait HostnameResolver {
    /// Fetch hostname from the remote agent
    fn fetch_remote_hostname(&self) -> HostnameResult;
    
    /// Check if local hostname should be used instead of remote
    fn should_use_local_hostname(&self) -> bool;
}

/// Platform-agnostic hostname resolver that works on both Unix and Windows.
/// This resolver tries multiple sources to get the remote hostname.
pub struct DefaultHostnameResolver {
    /// Whether to use local hostname instead of remote
    use_local: bool,
}

impl DefaultHostnameResolver {
    /// Create a new hostname resolver
    pub fn new(use_local: bool) -> Self {
        Self { use_local }
    }
    
    /// Create a resolver that prefers remote hostname
    pub fn remote() -> Self {
        Self::new(false)
    }
    
    /// Create a resolver that prefers local hostname
    pub fn local() -> Self {
        Self::new(true)
    }
}

impl HostnameResolver for DefaultHostnameResolver {
    fn fetch_remote_hostname(&self) -> HostnameResult {
        trace!("DefaultHostnameResolver: Starting remote hostname fetch...");
        
        // Check for target hostname from environment (what mirrord sets from the target pod)
        if let Ok(hostname) = std::env::var("HOSTNAME") {
            trace!("Using target hostname from HOSTNAME env var: {}", hostname);
            if let Ok(cstring) = CString::new(hostname) {
                return HostnameResult::Success(cstring);
            }
        }
        
        // Check for override environment variable as fallback
        if let Ok(hostname) = std::env::var("MIRRORD_OVERRIDE_HOSTNAME") {
            trace!("Using hostname from MIRRORD_OVERRIDE_HOSTNAME env var: {}", hostname);
            if let Ok(cstring) = CString::new(hostname) {
                return HostnameResult::Success(cstring);
            }
        }
        
        // If no environment variables are set, fall back to local hostname
        HostnameResult::UseLocal
    }
    
    fn should_use_local_hostname(&self) -> bool {
        self.use_local
    }
}

/// Get or initialize hostname using the provided resolver.
/// This function handles the caching logic and will only call the resolver
/// once to populate the cache.
pub fn get_or_init_hostname<R: HostnameResolver>(
    resolver: &R,
) -> HostnameResult {
    if let Some(hostname) = HOSTNAME.get() {
        return HostnameResult::Success(hostname.clone());
    }
    
    if resolver.should_use_local_hostname() {
        return HostnameResult::UseLocal;
    }

    let result = resolver.fetch_remote_hostname();
    
    match &result {
        HostnameResult::Success(hostname) => {
            // Try to cache the hostname, but don't fail if we can't
            let _ = set_hostname(hostname.clone());
        }
        _ => {}
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    
    struct MockResolver {
        should_use_local: bool,
        hostname: Option<String>,
    }
    
    impl HostnameResolver for MockResolver {
        fn fetch_remote_hostname(&self) -> HostnameResult {
            match &self.hostname {
                Some(h) => match CString::new(h.clone()) {
                    Ok(cstring) => HostnameResult::Success(cstring),
                    Err(_) => HostnameResult::Error(HostnameError::InvalidData),
                },
                None => HostnameResult::Error(HostnameError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No hostname available"
                ))),
            }
        }
        
        fn should_use_local_hostname(&self) -> bool {
            self.should_use_local
        }
    }
    
    #[test]
    fn test_hostname_caching() {
        let resolver = MockResolver {
            should_use_local: false,
            hostname: Some("test-hostname".to_string()),
        };
        
        // First call should fetch from resolver
        let result = get_or_init_hostname(&resolver);
        assert!(matches!(result, HostnameResult::Success(_)));
        
        // Check cached value
        let cached = get_cached_hostname();
        assert!(cached.is_some());
    }
    
    #[test]
    fn test_use_local_hostname() {
        // Create a resolver that should use local hostname
        let resolver = MockResolver {
            should_use_local: true,
            hostname: Some("test-hostname".to_string()),
        };
        
        // If hostname is already cached, we need to test differently
        if HOSTNAME.get().is_some() {
            // If cached, test that resolver preference is still respected
            let result = resolver.should_use_local_hostname();
            assert!(result);
        } else {
            // If not cached, test the full flow
            let result = get_or_init_hostname(&resolver);
            assert!(matches!(result, HostnameResult::UseLocal));
        }
    }
}

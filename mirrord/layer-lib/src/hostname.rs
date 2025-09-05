//! Hostname resolution functionality shared between Unix layer and Windows layer-win.
//!
//! This module provides a cross-platform interface for managing hostname resolution
//! that can be used by both the Unix layer and the Windows layer-win.

use std::ffi::CString;

use tracing::trace;

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
            trace!(
                "Using hostname from MIRRORD_OVERRIDE_HOSTNAME env var: {}",
                hostname
            );
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

/// Get hostname using the provided resolver.
/// This function always calls the resolver to get the most up-to-date hostname
/// instead of using any cached values.
pub fn get_hostname<R: HostnameResolver>(resolver: &R) -> HostnameResult {
    if resolver.should_use_local_hostname() {
        return HostnameResult::UseLocal;
    }

    resolver.fetch_remote_hostname()
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
                    "No hostname available",
                ))),
            }
        }

        fn should_use_local_hostname(&self) -> bool {
            self.should_use_local
        }
    }

    #[test]
    fn test_hostname_resolution() {
        let resolver = MockResolver {
            should_use_local: false,
            hostname: Some("test-hostname".to_string()),
        };

        // Call should fetch from resolver
        let result = get_hostname(&resolver);
        assert!(matches!(result, HostnameResult::Success(_)));
    }

    #[test]
    fn test_use_local_hostname() {
        // Create a resolver that should use local hostname
        let resolver = MockResolver {
            should_use_local: true,
            hostname: Some("test-hostname".to_string()),
        };

        // Test that resolver preference is respected
        let result = get_hostname(&resolver);
        assert!(matches!(result, HostnameResult::UseLocal));
    }
}

//! Unix-specific hostname resolver implementation compatible with Windows
//! 
//! This module provides the Unix-specific implementation of hostname resolution
//! that integrates with the existing layer infrastructure through trait boundaries.
//! Since the remote target is always Linux, this resolver works for both Unix and Windows clients.

use std::{ffi::CString, path::PathBuf};
use tracing::trace;

use crate::{hostname::{HostnameResolver, HostnameResult, HostnameError}, proxy_connection::ProxyConnection};

/// Unix-specific hostname resolver that fetches hostname from remote /etc/hostname
/// Works for both Unix and Windows clients since remote target is always Linux
pub struct UnixHostnameResolver<'a> {
    /// Whether to use local hostname instead of remote
    pub use_local: bool,
    /// Optional proxy connection for remote hostname fetching
    pub proxy_connection: Option<&'a ProxyConnection>,
}

impl<'a> HostnameResolver for UnixHostnameResolver<'a> {
    fn fetch_remote_hostname(&self) -> HostnameResult {
        if self.should_use_local_hostname() {
            return HostnameResult::UseLocal;
        }

        if let Ok(hostname) = std::env::var("MIRRORD_OVERRIDE_HOSTNAME") {
            trace!("UnixHostnameResolver: Using hostname from MIRRORD_OVERRIDE_HOSTNAME env var: {}", hostname);
            if let Ok(cstring) = CString::new(hostname) {
                return HostnameResult::Success(cstring);
            }
        }

        // Try to fetch hostname from remote /etc/hostname via ProxyConnection
        if let Some(proxy) = &self.proxy_connection {
            trace!("UnixHostnameResolver: Attempting to fetch hostname via ProxyConnection from /etc/hostname");
            match self.fetch_hostname_via_proxy(proxy) {
                Ok(hostname) => {
                    trace!("UnixHostnameResolver: Successfully fetched hostname via proxy: {}", hostname);
                    match CString::new(hostname) {
                        Ok(cstring) => return HostnameResult::Success(cstring),
                        Err(_) => {
                            trace!("UnixHostnameResolver: Invalid hostname data from proxy");
                            return HostnameResult::Error(HostnameError::InvalidData);
                        }
                    }
                }
                Err(e) => {
                    trace!("UnixHostnameResolver: Failed to fetch hostname via proxy: {}", e);
                    // Don't fail completely, fall through to local methods
                }
            }
        }

        // If ProxyConnection method fails, indicate we should use local hostname
        trace!("UnixHostnameResolver: No proxy connection available or failed, using local hostname");
        HostnameResult::UseLocal
    }
    
    fn should_use_local_hostname(&self) -> bool {
        self.use_local
    }
}

impl<'a> UnixHostnameResolver<'a> {
    /// Create a new resolver that uses remote hostname fetching via ProxyConnection
    pub fn remote(proxy: &'a ProxyConnection) -> Self {
        Self {
            use_local: false,
            proxy_connection: Some(proxy),
        }
    }

    /// Create a new resolver that uses local hostname
    pub fn local() -> Self {
        Self {
            use_local: true,
            proxy_connection: None,
        }
    }

    /// Fetch hostname from remote target via ProxyConnection by reading /etc/hostname
    fn fetch_hostname_via_proxy(&self, proxy: &ProxyConnection) -> Result<String, HostnameError> {
        use mirrord_protocol::file::{OpenFileRequest, ReadFileRequest, CloseFileRequest, OpenOptionsInternal};

        // Open /etc/hostname on the remote target
        let open_request = OpenFileRequest {
            path: PathBuf::from("/etc/hostname"),
            open_options: OpenOptionsInternal {
                read: true,
                ..Default::default()
            },
        };

        let open_response = match proxy.make_request_with_response(open_request) {
            Ok(response) => response,
            Err(e) => return Err(HostnameError::Protocol(format!("Failed to open /etc/hostname: {e}"))),
        };

        let fd = match open_response {
            Ok(open_file_response) => open_file_response.fd,
            Err(e) => return Err(HostnameError::Protocol(format!("Remote error opening /etc/hostname: {e:?}"))),
        };

        // Read the hostname content
        let read_request = ReadFileRequest {
            remote_fd: fd,
            buffer_size: 256, // Should be enough for a hostname
        };

        let read_response = match proxy.make_request_with_response(read_request) {
            Ok(response) => response,
            Err(e) => {
                // Try to close the file even if read failed
                let _ = proxy.make_request_no_response(CloseFileRequest { fd });
                return Err(HostnameError::Protocol(format!("Failed to read /etc/hostname: {e}")));
            }
        };

        let hostname_bytes = match read_response {
            Ok(read_file_response) => read_file_response.bytes,
            Err(e) => {
                // Try to close the file even if read failed
                let _ = proxy.make_request_no_response(CloseFileRequest { fd });
                return Err(HostnameError::Protocol(format!("Remote error reading /etc/hostname: {e:?}")));
            }
        };

        // Close the file
        let _ = proxy.make_request_no_response(CloseFileRequest { fd });

        // Convert bytes to string and trim whitespace
        let hostname = String::from_utf8_lossy(&hostname_bytes).trim().to_string();
        
        if hostname.is_empty() {
            return Err(HostnameError::Protocol("Empty hostname from /etc/hostname".to_string()));
        }

        Ok(hostname)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn test_unix_hostname_resolver_creation() {
        let local_resolver = UnixHostnameResolver::local();
        assert!(local_resolver.should_use_local_hostname());

        // For testing remote resolver, we would need a mock ProxyConnection
        // This test verifies that local resolver works properly
        assert!(local_resolver.proxy_connection.is_none());
    }

    #[test] 
    fn test_environment_variable_hostname() {
        std::env::set_var("MIRRORD_OVERRIDE_HOSTNAME", "test-hostname");
        
        // Create a resolver without proxy connection to test env var fallback
        let resolver = UnixHostnameResolver {
            use_local: false,
            proxy_connection: None,
        };
        match resolver.fetch_remote_hostname() {
            HostnameResult::Success(hostname) => {
                assert_eq!(hostname.to_string_lossy(), "test-hostname");
            }
            _ => panic!("Expected hostname from environment variable"),
        }
        
        std::env::remove_var("MIRRORD_OVERRIDE_HOSTNAME");
    }

    #[test]
    fn test_override_hostname() {
        std::env::set_var("MIRRORD_OVERRIDE_HOSTNAME", "override-hostname");
        
        // Create a resolver without proxy connection to test env var fallback
        let resolver = UnixHostnameResolver {
            use_local: false,
            proxy_connection: None,
        };
        match resolver.fetch_remote_hostname() {
            HostnameResult::Success(hostname) => {
                assert_eq!(hostname.to_string_lossy(), "override-hostname");
            }
            _ => panic!("Expected hostname from override environment variable"),
        }
        
        std::env::remove_var("MIRRORD_OVERRIDE_HOSTNAME");
    }

    #[test]
    fn test_local_hostname_preference() {
        let resolver = UnixHostnameResolver::local();
        match resolver.fetch_remote_hostname() {
            HostnameResult::UseLocal => {},
            _ => panic!("Expected UseLocal result for local resolver"),
        }
    }
}

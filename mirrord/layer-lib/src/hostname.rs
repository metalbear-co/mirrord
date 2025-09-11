//! Hostname resolution functionality shared between Unix layer and Windows layer-win.
//!
//! This module provides a cross-platform interface for managing hostname resolution
//! that can be used by both the Unix layer and the Windows layer-win.

use std::ffi::CString;

use tracing::trace;

use crate::{
    common::proxy_connection::{make_proxy_request_no_response, make_proxy_request_with_response},
    error::HookResult,
};

/// Efficient result type for internal file operations
type FileResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// RAII wrapper for remote file operations that automatically closes the file when dropped
struct ManagedRemoteFile {
    fd: u64,
}

impl ManagedRemoteFile {
    /// Open a remote file for reading
    fn open(file_path: &str) -> FileResult<Self> {
        use std::path::PathBuf;

        use mirrord_protocol::file::{OpenFileRequest, OpenOptionsInternal};

        let open_request = OpenFileRequest {
            path: PathBuf::from(file_path),
            open_options: OpenOptionsInternal {
                read: true,
                write: false,
                append: false,
                truncate: false,
                create: false,
                create_new: false,
            },
        };

        let fd = make_proxy_request_with_response(open_request)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
            .fd;

        Ok(Self { fd })
    }

    /// Read the entire file content up to max_size bytes
    fn read_all(&self, max_size: u64) -> FileResult<Vec<u8>> {
        use mirrord_protocol::file::ReadFileRequest;

        let read_request = ReadFileRequest {
            remote_fd: self.fd,
            buffer_size: max_size,
        };

        let config_bytes = make_proxy_request_with_response(read_request)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
            .bytes
            .to_vec();

        Ok(config_bytes)
    }
}

impl Drop for ManagedRemoteFile {
    fn drop(&mut self) {
        // Ignore any errors during cleanup - we don't want to panic in Drop
        let _ = make_proxy_request_no_response(mirrord_protocol::file::CloseFileRequest {
            fd: self.fd,
        });
    }
}

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

/// Generic helper to read a file from the remote target via ProxyConnection
fn read_remote_file_via_proxy(file_path: &str, max_size: u64) -> HookResult<Vec<u8>> {
    let result: FileResult<Vec<u8>> = (|| {
        let file = ManagedRemoteFile::open(file_path)?;
        file.read_all(max_size)
    })();

    result.map_err(|e| {
        crate::error::HookError::IO(std::io::Error::other(format!(
            "Failed to read file {}: {}",
            file_path, e
        )))
    })
}

/// Fetch Samba NetBIOS configuration from remote target via ProxyConnection by reading
/// /etc/samba/smb.conf
pub fn fetch_samba_config_via_proxy() -> HookResult<Option<String>> {
    // Read /etc/samba/smb.conf from the remote target (max 64KB should be enough)
    let config_bytes = read_remote_file_via_proxy("/etc/samba/smb.conf", 65536)?;

    // Parse the configuration to find NetBIOS name
    let config_content = String::from_utf8_lossy(&config_bytes);
    let netbios_name = parse_samba_netbios_name(&config_content);

    Ok(netbios_name)
}

/// Parse Samba configuration content to extract NetBIOS name
///
/// Looks for patterns like:
/// - netbios name = HOSTNAME
/// - netbios name=HOSTNAME
/// - workgroup = WORKGROUP (fallback)
pub fn parse_samba_netbios_name(config: &str) -> Option<String> {
    let mut in_global_section = false;
    let mut netbios_name = None;
    let mut workgroup = None;

    for line in config.lines() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Check for section headers
        if line.starts_with('[') && line.ends_with(']') {
            in_global_section = line.eq_ignore_ascii_case("[global]");
            continue;
        }

        // Only process global section
        if !in_global_section {
            continue;
        }

        // Look for netbios name setting
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_lowercase();
            let value = value.trim().trim_matches('"').trim_matches('\'');

            match key.as_str() {
                "netbios name" => {
                    if !value.is_empty() {
                        netbios_name = Some(value.to_uppercase());
                        // netbios name takes precedence
                        break;
                    }
                }
                "workgroup" => {
                    if !value.is_empty() && workgroup.is_none() {
                        workgroup = Some(value.to_uppercase());
                    }
                }
                _ => {}
            }
        }
    }

    // Return netbios name if found, otherwise workgroup as fallback
    netbios_name.or(workgroup)
}

/// Perform remote DNS resolution via ProxyConnection using mirrord protocol
pub fn remote_dns_resolve_via_proxy(hostname: &str) -> HookResult<Vec<(String, std::net::IpAddr)>> {
    use mirrord_protocol::dns::{AddressFamily, GetAddrInfoRequestV2, LookupRecord, SockType};

    let request = GetAddrInfoRequestV2 {
        node: hostname.to_string(),
        service_port: 0,
        flags: 0,
        family: AddressFamily::Any,
        socktype: SockType::Any,
        protocol: 0,
    };

    // DNS requests have a different response type, so we need to handle them directly
    let addr_info_list = make_proxy_request_with_response(request)?.0?;
    Ok(addr_info_list
        .into_iter()
        .map(|LookupRecord { name, ip }| (name, ip))
        .collect())
}

//! Hostname resolution functionality shared between Unix layer and Windows layer-win.
//!
//! This module provides a cross-platform interface for managing hostname resolution
//! that can be used by both the Unix layer and the Windows layer-win.

use tracing::trace;

use crate::{
    error::{HookResult, HostnameResolveError},
    proxy_connection::{make_proxy_request_no_response, make_proxy_request_with_response},
    setup::setup,
};

/// RAII wrapper for remote file operations that automatically closes the file when dropped
struct ManagedRemoteFile {
    path: String,
    fd: u64,
}

impl ManagedRemoteFile {
    /// Open a remote file for reading
    fn open(file_path: &str) -> HookResult<Self> {
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

        let fd = make_proxy_request_with_response(open_request)?
            .map_err(|e| HostnameResolveError::FileOpenError {
                path: file_path.to_string(),
                details: e.to_string(),
            })?
            .fd;

        Ok(Self {
            fd,
            path: file_path.to_string(),
        })
    }

    /// Read the entire file content up to max_size bytes
    fn read_all(&self, max_size: u64) -> HookResult<Vec<u8>> {
        use mirrord_protocol::file::ReadFileRequest;

        let read_request = ReadFileRequest {
            remote_fd: self.fd,
            buffer_size: max_size,
        };

        let config_bytes = make_proxy_request_with_response(read_request)?
            .map_err(|e| HostnameResolveError::FileReadError {
                path: self.path.clone(),
                details: e.to_string(),
            })?
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

/// Retrieves the `hostname` from the agent's `/etc/hostname` to be used by [`libc::gethostname`]
/// Returns Ok(None) on Bypassed remote hostname retrieval due to setup::local_hostname
/// configuration
pub fn remote_hostname_string(check_is_enabled: bool) -> HookResult<Option<String>> {
    // Check if hostname feature is enabled
    if !check_is_enabled {
        tracing::trace!("Skipping hostname feature check");
    } else if setup().local_hostname() {
        tracing::debug!("Hostname feature disabled");
        return Ok(None);
    }

    if let Ok(hostname) = std::env::var("MIRRORD_OVERRIDE_HOSTNAME") {
        trace!(
            "Using hostname from MIRRORD_OVERRIDE_HOSTNAME env var: {}",
            hostname
        );
        return Ok(Some(hostname));
    }

    // Try to fetch hostname from remote /etc/hostname via ProxyConnection
    // hostnames should never exceed 256 bytes
    let hostname_bytes = read_remote_file_via_proxy("/etc/hostname", 256)?;
    let raw_hostname = String::from_utf8_lossy(&hostname_bytes).to_string();
    let hostname = raw_hostname.trim_end().to_string();
    trace!("Successfully fetched hostname via proxy: {}", hostname);
    Ok(Some(hostname))
}

/// Generic helper to read a file from the remote target via ProxyConnection
fn read_remote_file_via_proxy(file_path: &str, max_size: u64) -> HookResult<Vec<u8>> {
    ManagedRemoteFile::open(file_path)?.read_all(max_size)
}

/// Fetch Samba NetBIOS configuration from remote target via ProxyConnection by reading
/// /etc/samba/smb.conf
pub fn get_remote_netbios_name() -> HookResult<Option<String>> {
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
fn parse_samba_netbios_name(config: &str) -> Option<String> {
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

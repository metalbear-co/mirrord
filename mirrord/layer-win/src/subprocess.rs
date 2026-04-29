//! Windows subprocess handling for mirrord layer.
//!
//! This module provides functionality for detecting and handling child/parent process
//! relationships in the Windows layer, enabling proper layer state inheritance between
//! processes.

use std::{net::SocketAddr, time::Duration};

use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    proxy_connection::ProxyConnection,
};

/// Environment variable for child process parent PID
const MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID: &str = "MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID";

/// Environment variable for child process layer ID
const MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID: &str = "MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID";

/// Environment variable for child process proxy address
const MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR: &str = "MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR";

/// Default timeout for proxy connections (30 seconds)
const DEFAULT_PROXY_CONNECTION_TIMEOUT_SECS: u64 = 30;

/// Represents the context of the current process (parent or child)
#[derive(Debug)]
pub enum ProcessContext {
    Parent,
    Child {
        parent_pid: u32,
        layer_id: u64,
        proxy_addr: SocketAddr,
    },
}

/// Detect whether this is a parent or child process based on environment variables
pub fn detect_process_context() -> LayerResult<ProcessContext> {
    if let (Ok(parent_pid_str), Ok(parent_layer_id_str), Ok(proxy_addr_str)) = (
        std::env::var(MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID),
        std::env::var(MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID),
        std::env::var(MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR),
    ) {
        let parent_pid: u32 = parent_pid_str.parse().map_err(|_| {
            LayerError::ProcessSynchronization("Invalid parent PID format".to_string())
        })?;

        let layer_id: u64 = parent_layer_id_str.parse().map_err(|_| {
            LayerError::ProcessSynchronization("Invalid parent layer ID format".to_string())
        })?;

        let proxy_addr: SocketAddr = proxy_addr_str.parse().map_err(|_| {
            LayerError::ProcessSynchronization("Invalid proxy address format".to_string())
        })?;

        Ok(ProcessContext::Child {
            parent_pid,
            layer_id,
            proxy_addr,
        })
    } else {
        Ok(ProcessContext::Parent)
    }
}

/// Create a proxy connection based on the process context
pub fn create_proxy_connection(context: &ProcessContext) -> LayerResult<ProxyConnection> {
    let current_pid = std::process::id();
    let timeout = Duration::from_secs(DEFAULT_PROXY_CONNECTION_TIMEOUT_SECS);

    match context {
        ProcessContext::Child {
            parent_pid,
            layer_id,
            proxy_addr,
        } => {
            // This is a child process - handle inheritance similar to Unix fork_detour
            tracing::debug!(
                "Child process {} initializing layer with parent inheritance",
                current_pid
            );

            let process_info = create_process_info(current_pid, *parent_pid);

            // Create session request with parent layer inheritance (similar to Unix fork_detour)
            let session = mirrord_intproxy_protocol::NewSessionRequest {
                parent_layer: Some(mirrord_intproxy_protocol::LayerId(*layer_id)),
                process_info,
            };

            let connection = ProxyConnection::new(*proxy_addr, session, timeout)
                .map_err(LayerError::ProxyConnectionFailed)?;

            tracing::debug!(
                "Child process {} successfully inherited layer state from parent {}",
                current_pid,
                parent_pid
            );
            Ok(connection)
        }
        ProcessContext::Parent => {
            // This is a parent process - standard initialization
            tracing::debug!("Parent process {} initializing layer", current_pid);

            let process_info = create_process_info(current_pid, 0);

            // Use the same environment variable as Unix layer
            let address = std::env::var(mirrord_config::MIRRORD_LAYER_INTPROXY_ADDR)
                .map_err(LayerError::MissingEnvIntProxyAddr)?
                .parse::<SocketAddr>()
                .map_err(LayerError::MalformedIntProxyAddr)?;

            // Set up session request - no parent layer for parent process
            let session = mirrord_intproxy_protocol::NewSessionRequest {
                parent_layer: None,
                process_info,
            };

            ProxyConnection::new(address, session, timeout)
                .map_err(LayerError::ProxyConnectionFailed)
        }
    }
}

/// Create process info for the current process
fn create_process_info(pid: u32, parent_pid: u32) -> mirrord_intproxy_protocol::ProcessInfo {
    mirrord_intproxy_protocol::ProcessInfo {
        pid: pid as _,
        parent_pid: parent_pid as _,
        name: get_current_process_name(),
        cmdline: std::env::args().collect(),
        loaded: true,
    }
}

/// Get the current process name, falling back to "unknown" if unable to determine
fn get_current_process_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name()?.to_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

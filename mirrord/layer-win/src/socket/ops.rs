use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse};
// Re-export shared types from layer-lib
pub use mirrord_layer_lib::socket::{
    ConnectionThrough, HookResult, SocketDescriptor, SocketKind, SocketState, UserSocket,
    get_socket, is_ignored_port,
};
use mirrord_layer_lib::{
    ConnectError,
    socket::{
        DnsResolver,
        ops::{ConnectResult, call_connect_fn, connect_outgoing},
    },
};
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;
use str_win;
use winapi::{
    shared::{
        minwindef::INT,
        ws2def::{SOCKADDR, WSABUF},
    },
    um::winsock2::{SOCKET, SOCKET_ERROR, WSAGetLastError},
};
use windows_strings::PCWSTR;

use crate::socket::{hostname, utils::SocketAddressExtWin};

/// Wrapper around Windows WSABUF array for safe buffer handling
#[derive(Debug)]
pub struct WSABufferData {
    buffers: Vec<(*const u8, u32)>,
    total_length: usize,
}

impl WSABufferData {
    /// Create from raw WSABUF array pointer and count
    pub unsafe fn from_raw(lpBuffers: *const u8, dwBufferCount: u32) -> Option<Self> {
        if lpBuffers.is_null() || dwBufferCount == 0 {
            return None;
        }

        let wsabuf_array = lpBuffers as *const WSABUF;
        let mut buffers = Vec::with_capacity(dwBufferCount as usize);
        let mut total_length = 0usize;

        for i in 0..dwBufferCount {
            // SAFETY: We've verified that wsabuf_array is not null and i is within bounds
            let wsabuf = unsafe { &*wsabuf_array.add(i as usize) };
            if wsabuf.buf.is_null() {
                return None; // Invalid buffer
            }

            let buf_ptr = wsabuf.buf as *const u8;
            let buf_len = wsabuf.len;

            buffers.push((buf_ptr, buf_len));
            total_length += buf_len as usize;
        }

        Some(Self {
            buffers,
            total_length,
        })
    }

    /// Get the first buffer for simple single-buffer operations
    pub fn first_buffer(&self) -> Option<(*const u8, u32)> {
        self.buffers.first().copied()
    }

    /// Check if this is a simple single-buffer case
    pub fn is_single_buffer(&self) -> bool {
        self.buffers.len() == 1
    }

    /// Get total data length across all buffers
    pub fn total_length(&self) -> usize {
        self.total_length
    }

    /// Get number of buffers
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    /// Create a WSABUF for a single buffer (for calling original functions)
    pub fn create_single_wsabuf(&self, buffer: *const u8, length: u32) -> WSABUF {
        WSABUF {
            len: length,
            buf: buffer as *mut i8,
        }
    }
}

impl TryFrom<(*const u8, u32)> for WSABufferData {
    type Error = &'static str;

    fn try_from((lpBuffers, dwBufferCount): (*const u8, u32)) -> Result<Self, Self::Error> {
        // SAFETY: This is inherently unsafe since we're dealing with raw pointers
        // The caller must ensure the pointers are valid
        unsafe { Self::from_raw(lpBuffers, dwBufferCount) }.ok_or("Invalid WSABUF data")
    }
}

impl TryFrom<(*mut u8, u32)> for WSABufferData {
    type Error = &'static str;

    fn try_from((lpBuffers, dwBufferCount): (*mut u8, u32)) -> Result<Self, Self::Error> {
        // Convert *mut u8 to *const u8 and delegate to the const version
        Self::try_from((lpBuffers as *const u8, dwBufferCount))
    }
}

/// Windows-specific DNS resolver implementation
pub struct WindowsDnsResolver;

impl DnsResolver for WindowsDnsResolver {
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn resolve_hostname(
        &self,
        hostname: &str,
        _port: u16,
        _family: i32,
        _protocol: i32,
    ) -> Result<Vec<IpAddr>, Self::Error> {
        // Use the existing Windows remote DNS resolution
        match hostname::remote_dns_resolve(hostname) {
            Ok(records) => Ok(records.into_iter().map(|(_, ip)| ip).collect()),
            Err(e) => {
                tracing::debug!("Remote DNS resolution failed for {}: {}", hostname, e);
                // Fallback to local resolution as in Unix layer
                match (hostname, 0u16).to_socket_addrs() {
                    Ok(addresses) => Ok(addresses.map(|addr| addr.ip()).collect()),
                    Err(local_err) => {
                        tracing::debug!(
                            "Local DNS resolution also failed for {}: {}",
                            hostname,
                            local_err
                        );
                        Ok(vec![]) // No records found
                    }
                }
            }
        }
    }

    fn remote_dns_enabled(&self) -> bool {
        crate::layer_setup().remote_dns_enabled()
    }
}

/// Helper function to check if a UDP socket's remote address is reachable using GetNameInfoW
/// This is a workaround for WSASend not failing on unreachable addresses (due to UDP being
/// connectionless) Returns:
/// - 0 on success (address is reachable)
/// - Non-zero error code on failure (address unreachable or resolution failed)
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub fn check_address_reachability(remote_addr: &SocketAddress, socket: SOCKET) -> i32 {
    let Ok((sock_addr_raw, sock_addr_len)) = (unsafe { remote_addr.to_sockaddr() }) else {
        tracing::debug!(
            "check_address_reachability -> address conversion failed for socket {}",
            socket
        );
        return SOCKET_ERROR;
    };

    let mut node_buffer = [0u16; 256]; // Buffer for hostname
    let mut service_buffer = [0u16; 32]; // Buffer for service/port

    let result = unsafe {
        winapi::um::ws2tcpip::GetNameInfoW(
            &sock_addr_raw as *const SOCKADDR,
            sock_addr_len.try_into().unwrap(),
            node_buffer.as_mut_ptr(),
            node_buffer.len() as u32,
            service_buffer.as_mut_ptr(),
            service_buffer.len() as u32,
            //When the NI_NAMEREQD flag is set, a host name that cannot be resolved by the DNS
            // results in an error.
            winapi::shared::ws2def::NI_NAMEREQD, // | winapi::shared::ws2def::NI_DGRAM,
        )
    };

    if result != 0 {
        tracing::debug!(
            "check_address_reachability -> address resolution failed for socket {} with error {}, wsagetlasterror: {}",
            socket,
            result,
            unsafe { WSAGetLastError() }
        );
        // on failure, GetNameInfoW sets WSALastError
        return result;
    }

    // Successfully resolved - address is reachable
    tracing::debug!(
        "check_address_reachability -> address resolution successful for socket {}: node_buffer: {:?}",
        socket,
        unsafe { str_win::u16_buffer_to_string(PCWSTR(node_buffer.as_ptr()).as_wide()) }
    );

    0 // Success
}

/// Log connection result and return it
pub fn log_connection_result<T>(result: T, function_name: &str, addr: SockAddr)
where
    T: std::fmt::Display + std::cmp::PartialEq<i32>,
{
    let socket_address = addr.as_socket();
    if result == 0 {
        tracing::info!(
            "{} -> successfully connected to address: {:?}",
            function_name,
            socket_address
        );
    } else {
        tracing::error!(
            "{} -> failed to connect to address: {:?}, error code: {}, wsa_getlasterror: {}",
            function_name,
            addr,
            result,
            unsafe { WSAGetLastError() }
        );
    }
}

/// Attempt to establish a connection through the mirrord proxy using layer-lib
/// This integrates with the shared connect_outgoing logic from layer-lib
#[mirrord_layer_macro::instrument(level = "trace", skip(connect_fn), ret)]
pub fn connect_through_proxy_with_layer_lib<F>(
    socket: SOCKET,
    user_socket: Arc<UserSocket>,
    remote_addr: SocketAddr,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    let raw_remote_addr = SockAddr::from(remote_addr);
    let optional_ip_address = raw_remote_addr.as_socket();
    if let Some(ip_address) = optional_ip_address {
        if is_ignored_port(&ip_address) {
            return Err(ConnectError::BypassPort(ip_address.port()).into());
        }
    };

    // Determine the protocol based on the socket type
    let protocol = match user_socket.kind {
        SocketKind::Tcp(_) => NetProtocol::Stream,
        SocketKind::Udp(_) => NetProtocol::Datagrams,
    };

    // Check the outgoing selector to determine routing
    let resolver = WindowsDnsResolver;
    match crate::layer_setup()
        .outgoing_selector()
        .get_connection_through_with_resolver(remote_addr, protocol, &resolver)
    {
        Ok(ConnectionThrough::Remote(_filtered_addr)) => {
            tracing::debug!(
                "connect_through_proxy_with_layer_lib -> outgoing filter indicates remote connection for {:?}",
                remote_addr
            );
            // Continue with proxy connection using the filtered address
        }
        Ok(ConnectionThrough::Local(_)) => {
            tracing::debug!(
                "connect_through_proxy_with_layer_lib -> outgoing filter indicates local connection for {:?}, calling original",
                remote_addr
            );

            if check_address_reachability(&SocketAddress::from(remote_addr), socket) != 0 {
                return Err(ConnectError::AddressUnreachable(format!("{}", remote_addr)).into());
            }

            return call_connect_fn(connect_fn, socket, remote_addr.into(), None, None);
        }
        Err(e) => {
            tracing::warn!(
                "connect_through_proxy_with_layer_lib -> outgoing filter check failed: {}, falling back to original",
                e
            );
            return Err(ConnectError::Fallback.into());
        }
    }

    tracing::info!(
        "connect_through_proxy_with_layer_lib -> intercepting connection to {}",
        remote_addr
    );

    // Create the proxy request function that matches layer-lib expectations
    let proxy_request_fn =
        |request: OutgoingConnectRequest| -> HookResult<OutgoingConnectResponse> {
            match hostname::make_windows_proxy_request_with_response(request) {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(e)) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
                Err(e) => Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
            }
        };

    // Convert SocketAddr to SockAddr for layer-lib
    let remote_sock_addr = SockAddr::from(remote_addr);

    let enabled_tcp_outgoing = crate::layer_setup().outgoing_config().tcp;
    let enabled_udp_outgoing = crate::layer_setup().outgoing_config().udp;

    match NetProtocol::from(user_socket.kind) {
        NetProtocol::Datagrams if enabled_udp_outgoing => connect_outgoing(
            socket,
            remote_sock_addr,
            user_socket,
            NetProtocol::Datagrams,
            proxy_request_fn,
            connect_fn,
        ),

        NetProtocol::Stream => match user_socket.state {
            SocketState::Initialized | SocketState::Bound(..)
                if (optional_ip_address.is_some() && enabled_tcp_outgoing) =>
            {
                connect_outgoing(
                    socket,
                    remote_sock_addr,
                    user_socket,
                    protocol,
                    proxy_request_fn,
                    connect_fn,
                )
            }

            _ => Err(ConnectError::DisabledOutgoing(socket).into()),
        },

        _ => Err(ConnectError::DisabledOutgoing(socket).into()),
    }
}

/// Complete proxy connection flow that handles validation, conversion, and preparation
///
/// This function encapsulates the entire flow from raw sockaddr to prepared connection:
/// 1. Validates socket for outgoing traffic interception
/// 2. Converts Windows sockaddr to Rust SocketAddr
/// 3. Attempts proxy connection through mirrord
/// 4. Handles connection success and prepares final sockaddr
///
/// Returns either a prepared sockaddr ready for the original connect function,
/// or Fallback to indicate the caller should use the original function.
#[mirrord_layer_macro::instrument(level = "trace", skip(connect_fn), ret)]
pub fn attempt_proxy_connection<F>(
    socket: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    function_name: &str,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    use crate::socket::utils::SocketAddrExtWin;

    // Get the socket state (we know it exists from validation)
    let user_socket = match get_socket(socket) {
        Some(socket) => socket,
        None => {
            tracing::error!(
                "{} -> socket {} validated but not found in manager",
                function_name,
                socket
            );
            return Err(ConnectError::Fallback.into());
        }
    };

    // Convert Windows sockaddr to Rust SocketAddr
    let remote_addr = match SocketAddr::try_from_raw(name, namelen) {
        Some(addr) => addr,
        None => {
            tracing::warn!(
                "{} -> failed to convert sockaddr, falling back to original",
                function_name
            );
            return Err(ConnectError::Fallback.into());
        }
    };

    // Try to connect through the mirrord proxy using layer-lib integration
    connect_through_proxy_with_layer_lib(socket, user_socket, remote_addr, connect_fn)
}

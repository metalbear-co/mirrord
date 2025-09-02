use std::{net::SocketAddr, sync::Arc};

use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse};
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;

/// Platform-specific connect function types
#[cfg(windows)]
use winapi::shared::ws2def::SOCKADDR as SOCK_ADDR_T;
#[cfg(windows)]
use winapi::um::winsock2::SOCKET;

use crate::socket::{Connected, SocketState, UserSocket};

#[cfg(windows)]
#[allow(non_camel_case_types)]
pub type SOCK_ADDR_LEN_T = i32;

#[cfg(unix)]
#[allow(non_camel_case_types)]
pub type SOCKET = i32;
#[cfg(unix)]
#[allow(non_camel_case_types)]
pub type SOCK_ADDR_T = libc::sockaddr;
#[cfg(unix)]
#[allow(non_camel_case_types)]
pub type SOCK_ADDR_LEN_T = libc::socklen_t;

macro_rules! connect_fn_def {
    ($conv:expr) => {
        unsafe extern $conv fn (
            sockfd: SOCKET,
            addr: *const SOCK_ADDR_T,
            addrlen: SOCK_ADDR_LEN_T,
        ) -> i32
    };
}

// single xplatform ConnectFn definition of socket connect
#[cfg(windows)]
pub type ConnectFn = connect_fn_def!("system");
#[cfg(unix)]
pub type ConnectFn = connect_fn_def!("c");

/// Result type for connect operations that preserves errno information
#[derive(Debug)]
pub struct ConnectResult {
    result: i32,
    error: Option<i32>,
}

impl ConnectResult {
    pub fn new(result: i32, error: Option<i32>) -> Self {
        Self { result, error }
    }

    pub fn is_failure(&self) -> bool {
        self.error.is_some_and(|error| {
            #[cfg(unix)]
            {
                error != libc::EINTR && error != libc::EINPROGRESS
            }
            #[cfg(windows)]
            {
                const WSAEINTR: i32 = 10004;
                const WSAEINPROGRESS: i32 = 10036;
                error != WSAEINTR && error != WSAEINPROGRESS
            }
        })
    }

    pub fn result(&self) -> i32 {
        self.result
    }

    pub fn error(&self) -> Option<i32> {
        self.error
    }
}

impl From<i32> for ConnectResult {
    fn from(result: i32) -> Self {
        if result == -1 {
            ConnectResult {
                result,
                error: Some(get_last_error()),
            }
        } else {
            ConnectResult {
                result,
                error: None,
            }
        }
    }
}

impl From<ConnectResult> for i32 {
    fn from(connect_result: ConnectResult) -> Self {
        if let Some(error) = connect_result.error {
            set_last_error(error);
        }
        connect_result.result
    }
}

// Platform-specific error handling
#[cfg(unix)]
fn get_last_error() -> i32 {
    unsafe { *libc::__errno_location() }
}

#[cfg(unix)]
fn set_last_error(error: i32) {
    unsafe { *libc::__errno_location() = error };
}

#[cfg(windows)]
fn get_last_error() -> i32 {
    unsafe { winapi::um::winsock2::WSAGetLastError() }
}

#[cfg(windows)]
fn set_last_error(error: i32) {
    unsafe { winapi::um::winsock2::WSASetLastError(error) };
}

/// Error types for connect operations
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Socket not found: {0}")]
    SocketNotFound(SOCKET),
    #[error("Invalid socket state: {0}")]
    InvalidState(SOCKET),
    #[error("Address conversion error")]
    AddressConversion,
    #[error("Proxy request failed: {0}")]
    ProxyRequest(String),
    #[error("Fallback to original connection")]
    Fallback,
}

/// Common logic for outgoing connections that can be used by platform-specific implementations.
///
/// This function handles the mirrord proxy connection setup but leaves the actual socket
/// connection to the platform-specific implementation via the `connect_fn` callback.
///
/// ## Parameters
/// - `sockfd`: The socket file descriptor
/// - `remote_address`: The address the user wants to connect to
/// - `user_socket_info`: The socket information structure
/// - `protocol`: The network protocol (TCP/UDP)
/// - `connect_fn`: Platform-specific function to perform the actual socket connection
/// - `proxy_request_fn`: Function to make proxy requests to the agent
///
/// ## Returns
/// A `ConnectResult` containing the connection result and any error information
pub fn connect_outgoing<P, C>(
    sockfd: SOCKET,
    remote_address: SockAddr,
    mut user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
    connect_fn: C,
) -> Result<ConnectResult, ConnectError>
where
    P: FnOnce(OutgoingConnectRequest) -> Result<OutgoingConnectResponse, ConnectError>,
    C: FnOnce(SOCKET, SockAddr) -> ConnectResult,
{
    // Closure that performs the connection with mirrord messaging.
    let remote_connection = |remote_address: SockAddr| -> Result<ConnectResult, ConnectError> {
        // Prepare this socket to be intercepted.
        let remote_address =
            SocketAddress::try_from(remote_address).map_err(|_| ConnectError::AddressConversion)?;

        let request = OutgoingConnectRequest {
            remote_address: remote_address.clone(),
            protocol,
        };

        let response = proxy_request_fn(request)?;

        let OutgoingConnectResponse {
            layer_address,
            in_cluster_address,
            ..
        } = response;

        // Connect to the interceptor socket that is listening.
        let layer_address_sock = SockAddr::try_from(layer_address.clone())
            .map_err(|_| ConnectError::AddressConversion)?;

        let connect_result = connect_fn(sockfd, layer_address_sock);

        if connect_result.is_failure() {
            return Err(ConnectError::Io(std::io::Error::last_os_error()));
        }

        let connected = Connected {
            remote_address,
            local_address: in_cluster_address,
            layer_address: Some(layer_address),
        };

        // Update the socket state
        Arc::get_mut(&mut user_socket_info)
            .ok_or(ConnectError::InvalidState(sockfd))?
            .state = SocketState::Connected(connected);

        Ok(connect_result)
    };

    if remote_address.is_unix() {
        remote_connection(remote_address)
    } else {
        // For non-Unix sockets, we need to determine if this should be handled locally or remotely
        // This decision should be made by the platform-specific implementation based on
        // outgoing selector configuration
        remote_connection(remote_address)
    }
}

/// Creates an outgoing connection request for the specified address and protocol
pub fn create_outgoing_request(
    remote_address: SocketAddr,
    protocol: NetProtocol,
) -> OutgoingConnectRequest {
    OutgoingConnectRequest {
        remote_address: remote_address.into(),
        protocol,
    }
}

/// Updates a socket to connected state with the provided connection information
pub fn update_socket_connected_state(
    user_socket: &mut UserSocket,
    remote_address: SocketAddress,
    local_address: SocketAddress,
    layer_address: Option<SocketAddress>,
) {
    let connected = Connected {
        remote_address,
        local_address,
        layer_address,
    };

    user_socket.state = SocketState::Connected(connected);
}

/// Checks if an address should be handled as a Unix socket
pub fn is_unix_address(address: &SockAddr) -> bool {
    address.is_unix()
}

/// Converts a socket address to the appropriate format for outgoing connections
pub fn prepare_outgoing_address(address: SocketAddr) -> SocketAddress {
    address.into()
}

/// Version of connect_outgoing that performs the actual connect call when needed
pub fn connect_outgoing_with_call<P, F>(
    sockfd: SOCKET,
    remote_address: SockAddr,
    user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
    actual_connect_fn: F,
) -> Result<ConnectResult, ConnectError>
where
    F: FnOnce(SOCKET, SockAddr) -> ConnectResult,
    P: FnOnce(OutgoingConnectRequest) -> Result<OutgoingConnectResponse, ConnectError>,
{
    let connect_fn = |sockfd: SOCKET, addr: SockAddr| -> ConnectResult {
        #[cfg(unix)]
        let result = unsafe { actual_connect_fn(sockfd, addr.as_ptr(), addr.len()) };

        #[cfg(windows)]
        let result = actual_connect_fn(sockfd, addr);

        ConnectResult::from(result)
    };

    connect_outgoing(
        sockfd,
        remote_address,
        user_socket_info,
        protocol,
        proxy_request_fn,
        connect_fn,
    )
}

/// Version of connect_outgoing for UDP that doesn't perform actual connect (semantic connection
/// only)
pub fn connect_outgoing_udp<P>(
    sockfd: SOCKET,
    remote_address: SockAddr,
    user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
) -> Result<ConnectResult, ConnectError>
where
    P: FnOnce(OutgoingConnectRequest) -> Result<OutgoingConnectResponse, ConnectError>,
{
    let connect_fn = |_sockfd: SOCKET, _addr: SockAddr| -> ConnectResult {
        // For UDP, we don't actually connect, just return success
        ConnectResult::new(0, None)
    };

    connect_outgoing(
        sockfd,
        remote_address,
        user_socket_info,
        protocol,
        proxy_request_fn,
        connect_fn,
    )
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn test_connect_result_success() {
        let result = ConnectResult::from(0);
        assert!(!result.is_failure());
        assert_eq!(result.result(), 0);
        assert_eq!(result.error(), None);
    }

    #[test]
    fn test_connect_result_failure() {
        let result = ConnectResult::new(-1, Some(1)); // Generic error code
        assert!(result.is_failure());
        assert_eq!(result.result(), -1);
        assert_eq!(result.error(), Some(1));
    }

    #[test]
    fn test_create_outgoing_request() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let request = create_outgoing_request(addr, NetProtocol::Stream);

        assert_eq!(request.protocol, NetProtocol::Stream);
        // Additional assertions would depend on SocketAddress implementation
    }

    #[test]
    fn test_is_unix_address() {
        // This test would need a Unix socket address to be meaningful
        // For now, just test that the function exists and compiles
        let addr = SockAddr::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));
        assert!(!is_unix_address(&addr));
    }

    #[test]
    fn test_update_socket_connected_state() {
        use crate::socket::SocketKind;

        #[cfg(unix)]
        let (domain, socket_type) = (libc::AF_INET, libc::SOCK_STREAM);
        #[cfg(windows)]
        let (domain, socket_type) = (
            winapi::shared::ws2def::AF_INET,
            winapi::shared::ws2def::SOCK_STREAM,
        );

        let mut socket = UserSocket::new(
            domain,
            socket_type,
            0,
            SocketState::Initialized,
            SocketKind::Tcp(socket_type),
        );

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let local_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9090);

        update_socket_connected_state(&mut socket, remote_addr.into(), local_addr.into(), None);

        assert!(socket.is_connected());
        if let SocketState::Connected(connected) = &socket.state {
            assert_eq!(connected.layer_address, None);
        } else {
            panic!("Socket should be in connected state");
        }
    }
}

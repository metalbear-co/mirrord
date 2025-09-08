#[cfg(unix)]
use std::os::unix::io::RawFd;
use std::{net::SocketAddr, sync::Arc};

#[cfg(unix)]
use libc::{AF_UNIX, c_void, sockaddr, socklen_t};
use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse};
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;
use tracing::{debug, error, trace};
/// Platform-specific connect function types
#[cfg(windows)]
use winapi::shared::ws2def::SOCKADDR as SOCK_ADDR_T;
#[cfg(windows)]
use winapi::um::winsock2::SOCKET;

use super::sockets::set_socket_state;
use crate::{
    HookError, HookResult,
    error::ConnectError,
    socket::{Connected, SOCKETS, SocketState, UserSocket},
};

// Platform-specific socket descriptor type
#[cfg(unix)]
pub type SocketDescriptor = RawFd;
#[cfg(windows)]
pub type SocketDescriptor = SOCKET;

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
            sockfd: SocketDescriptor,
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
#[mirrord_layer_macro::instrument(
    level = "debug",
    ret,
    skip(_user_socket_info, proxy_request_fn, connect_fn)
)]
pub fn connect_outgoing<P, F>(
    sockfd: SocketDescriptor,
    remote_address: SockAddr,
    _user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    P: FnOnce(OutgoingConnectRequest) -> HookResult<OutgoingConnectResponse>,
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    // Closure that performs the connection with mirrord messaging.
    let remote_connection = |remote_addr: SockAddr| -> HookResult<ConnectResult> {
        // Prepare this socket to be intercepted.
        let remote_address = SocketAddress::try_from(remote_addr.clone()).unwrap();

        let request = OutgoingConnectRequest {
            remote_address: remote_address.clone(),
            protocol,
        };

        let response = match proxy_request_fn(request) {
            Ok(response) => response,
            Err(e) => return Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
        };

        let OutgoingConnectResponse {
            layer_address,
            in_cluster_address,
            ..
        } = response;

        // Connect to the interceptor socket that is listening.
        call_connect_fn(
            connect_fn,
            sockfd,
            remote_addr,
            Some(in_cluster_address),
            Some(layer_address),
        )
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
#[mirrord_layer_macro::instrument(level = "debug", ret, skip(connect_fn))]
pub fn call_connect_fn<F>(
    connect_fn: F,
    sockfd: SocketDescriptor,
    remote_addr: SockAddr,
    in_cluster_address_opt: Option<SocketAddress>,
    layer_address_opt: Option<SocketAddress>,
) -> HookResult<ConnectResult>
where
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    let remote_address = SocketAddress::try_from(remote_addr.clone()).unwrap();
    let (connect_result, connected) = match (
        layer_address_opt.clone(),
        in_cluster_address_opt.clone(),
    ) {
        (Some(layer_address), Some(in_cluster_address)) => {
            debug!(
                "call_connect_fn -> connecting to remote_address={:?}, in_cluster_address={:?}, layer_address={:?}",
                remote_address, in_cluster_address, layer_address
            );

            let layer_addr = SockAddr::try_from(layer_address.clone())?;
            let connect_result: ConnectResult = connect_fn(sockfd, layer_addr);
            if connect_result.is_failure() {
                error!(
                    "connect -> Failed call to connect_fn with {:#?}",
                    connect_result,
                );
                return Err(std::io::Error::last_os_error().into());
            }

            (
                connect_result,
                Connected {
                    remote_address: layer_address.clone(),
                    local_address: in_cluster_address,
                    layer_address: Some(layer_address),
                },
            )
        }
        _ => {
            // both or none of layer_address and in_cluster_address must be provided
            if layer_address_opt.is_none() ^ in_cluster_address_opt.is_none() {
                return Err(ConnectError::ParameterMissing(format!(
                    "layer_address: {:?}, in_cluster_address: {:?}",
                    layer_address_opt, in_cluster_address_opt
                ))
                .into());
            }

            debug!(
                "call_connect_fn -> connecting to remote_address={:?} (no mirrord interception)",
                remote_address
            );

            let connect_result: ConnectResult = connect_fn(sockfd, remote_addr);
            if connect_result.is_failure() {
                error!(
                    "connect -> Failed call to connect_fn with {:#?}",
                    connect_result,
                );
                return Err(std::io::Error::last_os_error().into());
            }

            (
                connect_result,
                Connected {
                    remote_address: remote_address.clone(),
                    local_address: remote_address,
                    layer_address: None,
                },
            )
        }
    };

    trace!("we are connected {connected:#?}");

    set_socket_state(sockfd, SocketState::Connected(connected));

    Ok(connect_result)
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
#[mirrord_layer_macro::instrument(
    level = "debug",
    ret,
    skip(user_socket_info, proxy_request_fn, actual_connect_fn)
)]
pub fn connect_outgoing_with_call<P, F>(
    sockfd: SocketDescriptor,
    remote_address: SockAddr,
    user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
    actual_connect_fn: F,
) -> HookResult<ConnectResult>
where
    P: FnOnce(OutgoingConnectRequest) -> HookResult<OutgoingConnectResponse>,
    F: FnOnce(SocketDescriptor, SockAddr) -> ConnectResult,
{
    let connect_fn = |sockfd: SocketDescriptor, addr: SockAddr| -> ConnectResult {
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
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(user_socket_info, proxy_request_fn))]
pub fn connect_outgoing_udp<P>(
    sockfd: SocketDescriptor,
    remote_address: SockAddr,
    user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
) -> HookResult<ConnectResult>
where
    P: FnOnce(OutgoingConnectRequest) -> HookResult<OutgoingConnectResponse>,
{
    // No-op connect function for UDP
    let connect_fn = |_sockfd: SocketDescriptor, _: SockAddr| -> ConnectResult {
        ConnectResult {
            result: 0,
            error: None,
        }
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

/// Helps manually resolving DNS on port `53` with UDP, see [`send_to`] and [`sendmsg`].
///
/// This function is used for DNS resolution handling and socket address mapping. When sending
/// UDP packets to non-port-53 destinations, it checks if the destination is one of our bound
/// sockets and returns the actual address if found.
///
/// ## Parameters
/// - `sockfd`: The socket file descriptor
/// - `user_socket_info`: Information about the socket making the request
/// - `destination`: The destination address being sent to
///
/// ## Returns
/// The actual socket address to send to, which may be different from the requested destination
/// if the destination is one of our managed sockets.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(user_socket_info))]
pub fn send_dns_patch(
    sockfd: SocketDescriptor,
    user_socket_info: Arc<UserSocket>,
    destination: SocketAddr,
) -> HookResult<SockAddr> {
    let mut sockets = SOCKETS.lock()?;
    // We want to keep holding this socket.
    sockets.insert(sockfd, user_socket_info);

    // Sending a packet on port NOT 53.
    let actual_destination = sockets
        .iter()
        .filter(|(_, socket)| socket.kind.is_udp())
        // Is the `destination` one of our sockets? If so, then we grab the actual address,
        // instead of the, possibly fake address from mirrord.
        .find_map(|(_, receiver_socket)| match &receiver_socket.state {
            SocketState::Bound(crate::socket::Bound {
                requested_address,
                address,
            }) => {
                // Special case for port `0`, see `getsockname`.
                if requested_address.port() == 0 {
                    (SocketAddr::new(requested_address.ip(), address.port()) == destination)
                        .then_some(*address)
                } else {
                    (*requested_address == destination).then_some(*address)
                }
            }
            SocketState::Connected(Connected {
                remote_address,
                layer_address,
                ..
            }) => {
                let remote_address: SocketAddr = remote_address.clone().try_into().ok()?;
                let layer_address: SocketAddr = layer_address.clone()?.try_into().ok()?;

                if remote_address == destination {
                    Some(layer_address)
                } else {
                    None
                }
            }
            _ => None,
        })
        .ok_or(HookError::ManagedSocketNotFound(destination))?;

    Ok(actual_destination.into())
}

/// Platform-specific sendto function type definitions
#[cfg(unix)]
pub type SendtoFn = unsafe extern "C" fn(
    sockfd: SocketDescriptor,
    buf: *const c_void,
    len: usize,
    flags: i32,
    dest_addr: *const sockaddr,
    addrlen: socklen_t,
) -> isize;

#[cfg(windows)]
pub type SendtoFn = unsafe extern "system" fn(
    s: SOCKET,
    buf: *const i8,
    len: i32,
    flags: i32,
    to: *const winapi::shared::ws2def::SOCKADDR,
    tolen: i32,
) -> i32;

/// ## DNS resolution on port `53`
///
/// There is a bit of trickery going on here, as this function first triggers a _semantical_
/// connection to an interceptor socket (we don't [`connect`] this `sockfd`, just change the
/// [`UserSocket`] state), and only then calls the actual [`sendto`] to send `raw_message` to
/// the interceptor address (instead of the `raw_destination`).
///
/// Once the [`UserSocket`] is in the [`Connected`] state, then any other [`sendto`] call with
/// a [`None`] `raw_destination` will call the original [`sendto`] to the bound `address`. A
/// similar logic applies to a [`Bound`] socket.
///
/// ## Usage
///
/// This function preserves the architecture and comments from the original Unix layer
/// implementation, providing a cross-platform interface for UDP sendto operations that
/// supports both DNS resolution (port 53) and regular UDP packet sending.
///
/// ## Parameters
/// - `sockfd`: Socket file descriptor
/// - `raw_message`: Pointer to the message buffer
/// - `message_length`: Length of the message
/// - `flags`: Send flags
/// - `raw_destination`: Pointer to destination address
/// - `sendto_fn`: Platform-specific sendto function
/// - `proxy_request_fn`: Function to make proxy requests
///
/// ## Returns
/// HookResult<isize>
#[mirrord_layer_macro::instrument(
    level = "debug",
    ret,
    skip(raw_message, destination, sendto_fn, proxy_request_fn)
)]
pub fn send_to<P, F>(
    sockfd: SocketDescriptor,
    raw_message: *const u8,
    message_length: usize,
    flags: i32,
    destination: SocketAddr,
    sendto_fn: F,
    proxy_request_fn: P,
) -> HookResult<isize>
where
    P: FnOnce(OutgoingConnectRequest) -> HookResult<OutgoingConnectResponse>,
    F: FnOnce(SocketDescriptor, *const u8, usize, i32, SockAddr) -> HookResult<isize>,
{
    let raw_destination = SockAddr::from(destination);
    trace!("destination {:?}", destination);

    let user_socket_info = SOCKETS
        .lock()?
        .remove(&sockfd)
        .ok_or(HookError::SocketNotFound(sockfd))?;

    // we don't support unix sockets which don't use `connect`
    #[cfg(unix)]
    if (destination.is_unix() || user_socket_info.domain == AF_UNIX)
        && !matches!(user_socket_info.state, SocketState::Connected(_))
    {
        return Err(HookError::UnsupportedSocketType);
    }

    // Currently this flow only handles DNS resolution.
    // So here we have to check for 2 things:
    //
    // 1. Are we sending something port 53? Then we use mirrord flow;
    // 2. Is the destination a socket that we have bound? Then we send it to the real address that
    // we've bound the destination socket.
    //
    // If none of the above are true, then the destination is some real address outside our scope.
    if let Some(destination) = raw_destination
        .as_socket()
        .filter(|destination| destination.port() != 53)
    {
        debug!(
            "layer-lib send_to -> non-DNS destination (port {}), checking for DNS patch",
            destination.port()
        );
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination.into())?;

        sendto_fn(
            sockfd,
            raw_message,
            message_length,
            flags,
            rawish_true_destination,
        )
    } else {
        connect_outgoing_udp(
            sockfd,
            destination.into(),
            user_socket_info,
            NetProtocol::Datagrams,
            proxy_request_fn,
        )?;

        let layer_address: SockAddr = SOCKETS
            .lock()?
            .get(&sockfd)
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => connected.layer_address.clone(),
                _ => unreachable!(),
            })
            .ok_or(HookError::ManagedSocketNotFound(destination))?
            .try_into()?;

        // Convert Windows parameters to cross-platform format
        sendto_fn(sockfd, raw_message, message_length, flags, layer_address)
    }
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

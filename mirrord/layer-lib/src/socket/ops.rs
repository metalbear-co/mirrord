#[cfg(unix)]
use std::os::unix::io::RawFd;
use std::{
    convert::TryFrom,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

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
#[cfg(windows)]
use winapi::um::winsock2::{WSA_IO_PENDING, WSAEINPROGRESS, WSAEINTR};

use super::sockets::{set_socket_state, socket_descriptor_to_i64};
#[cfg(windows)]
use crate::error::windows::WindowsError;
use crate::{
    HookError, HookResult,
    error::ConnectError,
    setup::setup,
    socket::{Bound, Connected, SOCKETS, SocketState, UserSocket, ConnectionThrough},
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
    ($conv:literal) => {
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
pub type ConnectFn = connect_fn_def!("C");

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
                error != WSAEINTR && error != WSAEINPROGRESS && error != WSA_IO_PENDING
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
fn errno_location() -> *mut libc::c_int {
    unsafe {
        #[cfg(target_os = "macos")]
        {
            libc::__error()
        }
        #[cfg(not(target_os = "macos"))]
        {
            libc::__errno_location()
        }
    }
}

fn get_last_error() -> i32 {
    #[cfg(unix)]
    unsafe {
        *errno_location()
    }

    #[cfg(windows)]
    match WindowsError::wsa_last_error() {
        WindowsError::WinSock(code) => code,
        _ => unreachable!(),
    }
}

fn set_last_error(error: i32) {
    #[cfg(unix)]
    unsafe {
        *errno_location() = error
    };

    #[cfg(windows)]
    unsafe {
        winapi::um::winsock2::WSASetLastError(error)
    };
}

fn nop_connect_fn(_addr: SockAddr) -> ConnectResult {
    ConnectResult {
        result: 0,
        error: None,
    }
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
    skip(user_socket_info, proxy_request_fn, connect_fn)
)]
pub fn connect_outgoing_common<P, F>(
    sockfd: SocketDescriptor,
    remote_address: SockAddr,
    user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    proxy_request_fn: P,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    P: FnOnce(OutgoingConnectRequest) -> HookResult<OutgoingConnectResponse>,
    F: FnOnce(SockAddr) -> ConnectResult,
{
    debug!("connect_outgoing -> preparing {protocol:?} connection to {remote_address:?}");

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
            connection_id,
            mut layer_address,
            in_cluster_address,
        } = response;

        if let SocketAddress::Ip(interceptor_addr) = &mut layer_address {
            // Our socket can be bound to any local interface,
            // so the interceptor listens on an unspecified IP address, e.g. 0.0.0.0
            // We need to fill the exact IP here.
            match &user_socket_info.state {
                SocketState::Bound {
                    bound: Bound { address, .. },
                    ..
                } => {
                    if interceptor_addr.ip().is_unspecified() {
                        if interceptor_addr.is_ipv4() {
                            interceptor_addr.set_ip(Ipv4Addr::LOCALHOST.into())
                        } else {
                            interceptor_addr.set_ip(Ipv6Addr::LOCALHOST.into())
                        }
                    } else {
                        interceptor_addr.set_ip(address.ip());
                    }
                }
                _ if interceptor_addr.is_ipv4() => {
                    interceptor_addr.set_ip(Ipv4Addr::LOCALHOST.into())
                }
                _ => interceptor_addr.set_ip(Ipv6Addr::LOCALHOST.into()),
            }
        }

        
        // Connect to the socket prepared by the internal proxy.
        let connect_result: ConnectResult = connect_fn(remote_addr);

        #[cfg(unix)]
        if let Some(raw_error) = connect_result.error
            && raw_error != libc::EINTR
            && raw_error != libc::EINPROGRESS
        {
            // We failed to connect to the internal proxy socket.
            // This is most likely a bug,
            // so let's try and include here as much info as possible.

            let error = io::Error::from_raw_os_error(raw_error);
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(sockfd) };
            let socket_name =
                nix::sys::socket::getsockname::<SockaddrStorage>(sockfd).map(|sockaddr| {
                    // `Debug` implementation is not very helpful, `ToString` produces a nice
                    // address.
                    sockaddr.to_string()
                });
            let socket_type = nix::sys::socket::getsockopt(&borrowed_fd, sockopt::SockType);
            error!(
                sockfd,
                ?user_socket_info,
                ?socket_type,
                socket_addr = ?socket_name,

                outgoing_connect_request_id = connection_id,
                internal_proxy_socket_address = %layer_address,
                agent_peer_address = %remote_address,
                agent_local_address = in_cluster_address.as_ref().map(ToString::to_string),

                raw_error,
                %error,

                ?SOCKETS,

                "Failed to connect to an internal proxy socket. \
                This is most likely a bug, please report it.",
            );
            return Detour::Error(error.into());
        }

        let connected = Connected {
            connection_id: Some(connection_id),
            remote_address,
            local_address: in_cluster_address,
            layer_address: Some(layer_address),
        };

        trace!("we are connected {connected:#?}");

        set_socket_state(sockfd, SocketState::Connected(connected));

        Ok(connect_result)
    };

    let connect_result = if remote_address.is_unix() {
        remote_connection(remote_address)?
    } else {
        // make sure its a valid IPV4/6 address
        let socket_addr = remote_address.as_socket().ok_or(HookError::UnsupportedSocketType)?;

        // Can't just connect to whatever `remote_address` is, as it might be a remotely resolved
        // address, in a local connection context (or vice versa), so we let `remote_connection`
        // handle this address trickery.
        match setup()
            .outgoing_selector()
            .get_connection_through(socket_addr, protocol)?
        {
            ConnectionThrough::Remote(addr) => remote_connection(SockAddr::from(addr))?,
            ConnectionThrough::Local(addr) => connect_fn(SockAddr::from(addr))
        }
    };
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

/// Checks if an address should be handled as a Unix socket
pub fn is_unix_address(address: &SockAddr) -> bool {
    address.is_unix()
}

/// Converts a socket address to the appropriate format for outgoing connections
pub fn prepare_outgoing_address(address: SocketAddr) -> SocketAddress {
    address.into()
}

/// Helps manually resolving DNS on port `53` with UDP, see `send_to` and `sendmsg`.
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
    let destination = sockets
        .iter()
        .filter(|(_, socket)| socket.kind.is_udp())
        // Is the `destination` one of our sockets? If so, then we grab the actual address,
        // instead of the possibly fake address from mirrord.
        .find_map(|(_, receiver_socket)| match &receiver_socket.state {
            SocketState::Bound {
                bound:
                    Bound {
                        requested_address,
                        address,
                    },
                ..
            } => {
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

    Ok(destination.into())
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
/// connection to an interceptor socket (we don't `connect` this `sockfd`, just change the
/// [`UserSocket`] state), and only then calls the actual `sendto` to send `raw_message` to
/// the interceptor address (instead of the `raw_destination`).
///
/// Once the [`UserSocket`] is in the [`Connected`] state, then any other `sendto` call with
/// a [`None`] `raw_destination` will call the original `sendto` to the bound `address`. A
/// similar logic applies to a `Bound` socket.
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
/// `HookResult<isize>`
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
        .ok_or_else(|| HookError::SocketNotFound(socket_descriptor_to_i64(sockfd)))?;

    // we don't support unix sockets which don't use `connect`
    #[cfg(unix)]
    if (raw_destination.is_unix() || user_socket_info.domain == AF_UNIX)
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
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination)?;

        sendto_fn(
            sockfd,
            raw_message,
            message_length,
            flags,
            rawish_true_destination,
        )
    } else {
        connect_outgoing_common(
            sockfd,
            destination.into(),
            user_socket_info,
            NetProtocol::Datagrams,
            proxy_request_fn,
            nop_connect_fn,
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

}
